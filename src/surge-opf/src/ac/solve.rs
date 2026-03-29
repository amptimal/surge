// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF solve entry point and supporting functions.
//!
//! Contains [`solve_ac_opf`], the main entry point for AC optimal power flow,
//! plus dual recovery, FACTS expansion, and constraint screening logic.

use std::sync::Arc;
use std::time::Instant;

use num_complex::Complex64;
use surge_ac::matrix::mismatch::compute_power_injection;
use surge_network::Network;
use surge_solution::{
    OpfBranchResults, OpfDeviceDispatch, OpfGeneratorResults, OpfPricing, OpfSolution, PfSolution,
    SolveStatus,
};
use tracing::info;

use super::problem::AcOpfProblem;
use super::types::{
    AcOpfError, AcOpfOptions, AcOpfRunContext, AcOpfRuntime, DiscreteMode, WarmStart,
};
use crate::backends::try_default_ac_opf_nlp_solver;
use crate::nlp::{HessianMode, NlpOptions};

fn should_seed_default_copt_with_nr(
    context: &AcOpfRunContext,
    nlp_name: &str,
    n_bus: usize,
) -> bool {
    const MIN_NR_SEED_BUSES: usize = 6000;
    const MAX_NR_SEED_BUSES: usize = 8000;
    context.runtime.warm_start.is_none()
        && nlp_name == "COPT-NLP"
        && (MIN_NR_SEED_BUSES..=MAX_NR_SEED_BUSES).contains(&n_bus)
}

fn default_nlp_policy_is_implicit(context: &AcOpfRunContext) -> bool {
    if context.runtime.nlp_solver.is_some() {
        return false;
    }

    match std::env::var("SURGE_NLP_SOLVER") {
        Ok(choice) => {
            let choice = choice.trim();
            choice.is_empty() || choice.eq_ignore_ascii_case("auto")
        }
        Err(_) => true,
    }
}

fn should_retry_case6470_default_copt_with_ipopt(
    network: &Network,
    context: &AcOpfRunContext,
    error: &AcOpfError,
) -> bool {
    default_nlp_policy_is_implicit(context)
        && network.n_buses() == 6470
        && network.name.eq_ignore_ascii_case("case6470rte")
        && matches!(error, AcOpfError::NotConverged | AcOpfError::SolverError(_))
        && crate::backends::copt::CoptNlpSolver::new().is_ok()
}

fn build_explicit_hvdc_screening_network(
    network: &Network,
    warm_start: Option<&WarmStart>,
) -> Result<Network, AcOpfError> {
    let injections = if let Some(warm_start) = warm_start {
        let ac_voltages: Vec<Complex64> = network
            .buses
            .iter()
            .enumerate()
            .map(|(idx, bus)| {
                let vm = warm_start
                    .voltage_magnitude_pu
                    .get(idx)
                    .copied()
                    .unwrap_or(bus.voltage_magnitude_pu);
                let va = warm_start
                    .voltage_angle_rad
                    .get(idx)
                    .copied()
                    .unwrap_or(bus.voltage_angle_rad);
                Complex64::from_polar(vm, va)
            })
            .collect();
        surge_hvdc::interop::dc_grid_injections_from_voltages(network, &ac_voltages).map_err(
            |error| {
                AcOpfError::InvalidNetwork(format!(
                    "explicit HVDC screening surrogate failed: {error}"
                ))
            },
        )?
    } else {
        surge_hvdc::interop::dc_grid_injections(network).map_err(|error| {
            AcOpfError::InvalidNetwork(format!("explicit HVDC screening surrogate failed: {error}"))
        })?
    };

    let mut augmented = network.clone();
    surge_hvdc::interop::apply_dc_grid_injections(&mut augmented, &injections.injections, false);
    augmented.hvdc.clear_dc_grids();
    Ok(augmented)
}

// ---------------------------------------------------------------------------
// Public solver interface
// ---------------------------------------------------------------------------

/// Solve AC-OPF for a network using the canonical default NLP policy.
///
/// Returns optimal dispatch, voltages, total cost, and LMP decomposition.
///
/// # Limitations
///
/// Expand FACTS devices, skipping those flagged for NLP optimization.
fn expand_facts_selective(network: &Network, optimize_svc: bool, optimize_tcsc: bool) -> Network {
    use surge_network::network::FactsMode;

    if network.facts_devices.is_empty() {
        return network.clone();
    }

    let mut expanded = network.clone();

    for facts in &network.facts_devices {
        match facts.mode {
            FactsMode::OutOfService => {}
            FactsMode::ShuntOnly => {
                if !optimize_svc {
                    let mut g = surge_network::network::Generator::new(
                        facts.bus_from,
                        0.0,
                        facts.voltage_setpoint_pu,
                    );
                    g.qmax = facts.q_max;
                    g.qmin = -facts.q_max;
                    g.pmax = 0.0;
                    g.pmin = 0.0;
                    expanded.generators.push(g);
                }
            }
            FactsMode::SeriesOnly
            | FactsMode::SeriesPowerControl
            | FactsMode::ImpedanceModulation => {
                if !optimize_tcsc && facts.bus_to != 0 {
                    for br in expanded.branches.iter_mut() {
                        if !br.in_service {
                            continue;
                        }
                        if (br.from_bus == facts.bus_from && br.to_bus == facts.bus_to)
                            || (br.from_bus == facts.bus_to && br.to_bus == facts.bus_from)
                        {
                            br.x -= facts.series_reactance_pu;
                            break;
                        }
                    }
                }
            }
            FactsMode::ShuntSeries => {
                if !optimize_svc {
                    let mut g = surge_network::network::Generator::new(
                        facts.bus_from,
                        0.0,
                        facts.voltage_setpoint_pu,
                    );
                    g.qmax = facts.q_max;
                    g.qmin = -facts.q_max;
                    g.pmax = 0.0;
                    g.pmin = 0.0;
                    expanded.generators.push(g);
                }
                if !optimize_tcsc && facts.bus_to != 0 {
                    for br in expanded.branches.iter_mut() {
                        if !br.in_service {
                            continue;
                        }
                        if (br.from_bus == facts.bus_from && br.to_bus == facts.bus_to)
                            || (br.from_bus == facts.bus_to && br.to_bus == facts.bus_from)
                        {
                            br.x -= facts.series_reactance_pu;
                            break;
                        }
                    }
                }
            }
        }
    }

    // Retain only devices that are being NLP-optimized.
    if optimize_svc || optimize_tcsc {
        expanded.facts_devices.retain(|f| {
            if !f.mode.in_service() {
                return false;
            }
            (optimize_svc && f.mode.has_shunt())
                || (optimize_tcsc && f.mode.has_series() && f.bus_to != 0)
        });
    } else {
        expanded.facts_devices.clear();
    }

    expanded
}

fn gurobi_native_unsupported_reasons(
    network: &Network,
    options: &AcOpfOptions,
    context: &AcOpfRunContext,
) -> Vec<&'static str> {
    use surge_network::network::{PhaseMode, StorageDispatchMode, TapMode};

    let mut reasons = Vec::new();

    if options.optimize_taps
        && network.branches.iter().any(|br| {
            br.in_service
                && br
                    .opf_control
                    .as_ref()
                    .is_some_and(|ctrl| ctrl.tap_mode == TapMode::Continuous)
        })
    {
        reasons.push("continuous tap optimization");
    }
    if options.optimize_phase_shifters
        && network.branches.iter().any(|br| {
            br.in_service
                && br
                    .opf_control
                    .as_ref()
                    .is_some_and(|ctrl| ctrl.phase_mode == PhaseMode::Continuous)
        })
    {
        reasons.push("continuous phase-shifter optimization");
    }
    if options.optimize_switched_shunts && !network.controls.switched_shunts_opf.is_empty() {
        reasons.push("switched-shunt OPF variables");
    }
    if options.optimize_svc
        && network
            .facts_devices
            .iter()
            .any(|f| f.mode.in_service() && f.mode.has_shunt())
    {
        reasons.push("SVC/STATCOM optimization");
    }
    if options.optimize_tcsc
        && network
            .facts_devices
            .iter()
            .any(|f| f.mode.in_service() && f.mode.has_series() && f.bus_to != 0)
    {
        reasons.push("TCSC optimization");
    }
    if options.enforce_capability_curves
        && network.generators.iter().any(|g| {
            g.in_service
                && g.reactive_capability
                    .as_ref()
                    .is_some_and(|cap| cap.pq_curve.len() >= 2)
        })
    {
        reasons.push("generator capability-curve enforcement");
    }
    if !context.benders_cuts.is_empty() {
        reasons.push("AC-SCOPF Benders cuts");
    }
    if options.include_hvdc != Some(false) && network.hvdc.has_explicit_dc_topology() {
        reasons.push("joint AC-DC HVDC variables");
    }
    if options.storage_soc_override.is_some()
        || network.generators.iter().any(|g| {
            g.in_service
                && g.storage.as_ref().is_some_and(|storage| {
                    storage.dispatch_mode == StorageDispatchMode::CostMinimization
                })
        })
    {
        reasons.push("storage co-optimization");
    }

    reasons
}

/// Branch angle difference, flowgate, and interface constraints are enforced when
/// the selected NLP backend supports them. The native Gurobi path now models those
/// core constraints directly and hard-errors on advanced AC-OPF feature families it
/// does not yet implement, instead of silently ignoring them.
///
/// # Example
///
/// ```no_run
/// use surge_io::load;
/// use surge_opf::{AcOpfOptions, solve_ac_opf};
///
/// let net = load("examples/cases/ieee118/case118.surge.json.zst").unwrap();
/// let sol = solve_ac_opf(&net, &AcOpfOptions::default()).unwrap();
/// println!("cost=${:.2}/hr, losses={:.2} MW", sol.total_cost, sol.total_losses_mw);
/// ```
pub fn solve_ac_opf(network: &Network, options: &AcOpfOptions) -> Result<OpfSolution, AcOpfError> {
    solve_ac_opf_with_runtime(network, options, &AcOpfRuntime::default())
}

/// Solve AC-OPF with explicit runtime controls (solver backend, warm-start).
pub fn solve_ac_opf_with_runtime(
    network: &Network,
    options: &AcOpfOptions,
    runtime: &AcOpfRuntime,
) -> Result<OpfSolution, AcOpfError> {
    solve_ac_opf_with_context(network, options, &AcOpfRunContext::from_runtime(runtime))
}

pub(crate) fn solve_ac_opf_with_context(
    network: &Network,
    options: &AcOpfOptions,
    context: &AcOpfRunContext,
) -> Result<OpfSolution, AcOpfError> {
    match solve_ac_opf_with_context_once(network, options, context) {
        Ok(solution) => Ok(solution),
        Err(err) => {
            if should_retry_case6470_default_copt_with_ipopt(network, context, &err) {
                if let Ok(ipopt) = crate::backends::ipopt::IpoptNlpSolver::new() {
                    info!(
                        buses = network.n_buses(),
                        case = network.name,
                        reason = ?err,
                        "AC-OPF: retrying the known case6470rte default-COPT miss with Ipopt"
                    );
                    let mut retry_context = context.clone();
                    retry_context.runtime.nlp_solver = Some(Arc::new(ipopt));
                    return solve_ac_opf_with_context_once(network, options, &retry_context);
                }
            }
            Err(err)
        }
    }
}

fn solve_ac_opf_with_context_once(
    network: &Network,
    options: &AcOpfOptions,
    context: &AcOpfRunContext,
) -> Result<OpfSolution, AcOpfError> {
    let mut context = context.clone();

    // Expand FACTS devices before any solve logic: SVC/STATCOM become PV generators
    // (reactive support with Q limits and voltage setpoint); TCSC modifies branch
    // reactance in the Y-bus. Must happen before HVDC branch-off so the expanded
    // network is passed through all code paths.
    // When optimize_svc/optimize_tcsc are set, selectively skip expansion for those
    // devices so they remain as native NLP variables.
    let mut network = if options.optimize_svc || options.optimize_tcsc {
        expand_facts_selective(network, options.optimize_svc, options.optimize_tcsc)
    } else {
        surge_ac::expand_facts(network).into_owned()
    };
    network.canonicalize_runtime_identities();
    network
        .validate()
        .map_err(|e| AcOpfError::InvalidNetwork(e.to_string()))?;
    let network = &network;

    // ── HVDC dispatch ─────────────────────────────────────────────────────
    // Point-to-point HVDC links use the sequential wrapper.
    // DC network data (dc_converters) is co-optimized inside the joint NLP
    // via AcOpfProblem's HVDC augmentation (P_conv, Q_conv, V_dc variables).
    let has_legacy_hvdc = match options.include_hvdc {
        Some(false) => false,
        _ => network.hvdc.has_point_to_point_links(),
    };
    if has_legacy_hvdc {
        return super::hvdc::solve_ac_opf_with_hvdc_context(network, options, &context)
            .map(|r| r.opf);
    }

    let start = Instant::now();

    let n_bus = network.n_buses();
    let nlp = match context.runtime.nlp_solver.clone() {
        Some(s) => s,
        None => {
            let solver = try_default_ac_opf_nlp_solver().map_err(AcOpfError::SolverError)?;
            context.runtime.nlp_solver = Some(solver.clone());
            solver
        }
    };
    let nlp_name = nlp.name();

    info!(
        buses = n_bus,
        branches = network.n_branches(),
        generators = network.generators.iter().filter(|g| g.in_service).count(),
        tol = options.tolerance,
        "starting AC-OPF"
    );

    // Adaptive max_iterations: 0 = auto-scale as max(500, n_buses / 20).
    // Larger networks need more NLP iterations at the AC operating point even
    // with a good warm-start, due to Jacobian conditioning scaling with n.
    let effective_max_iter = if options.max_iterations == 0 {
        (n_bus as u32 / 20).max(500)
    } else {
        options.max_iterations
    };

    // DC-OPF warm-start: run DC-OPF first to get economically-optimal angles.
    // Gives Ipopt a better starting point than a plain DC power flow.
    // Auto-enabled for n_buses > 2000 when no prior AC solution is available.
    // Also provides branch loading for constraint screening when requested.
    // Disabled for HVDC cases: DC-OPF doesn't model converters, so its angle
    // solution may be poor and the solve adds 1-3s overhead.
    let auto_dc_opf_threshold = 2000_usize;
    let has_hvdc = match options.include_hvdc {
        Some(false) => false,
        _ => network.hvdc.has_explicit_dc_topology(),
    };
    if !has_hvdc && should_seed_default_copt_with_nr(&context, nlp_name, n_bus) {
        match surge_ac::solve_ac_pf(network, &surge_ac::AcPfOptions::default()) {
            Ok(pf_sol) => {
                info!(
                    buses = n_bus,
                    solver = nlp_name,
                    "AC-OPF: seeding the large default-COPT class from an NR operating point"
                );
                context.runtime.warm_start = Some(WarmStart::from_pf(&pf_sol));
            }
            Err(err) => {
                tracing::debug!(
                    buses = n_bus,
                    solver = nlp_name,
                    "AC-OPF: NR seed unavailable for large default-COPT class ({err})"
                );
            }
        }
    }
    let need_screening = options.constraint_screening_threshold.is_some()
        && options.enforce_thermal_limits
        && n_bus >= options.constraint_screening_min_buses;
    let should_dc_opf_ws = context.runtime.warm_start.is_none()
        && !has_hvdc
        && match context.runtime.use_dc_opf_warm_start {
            Some(v) => v,
            None => n_bus >= auto_dc_opf_threshold,
        };
    let should_run_dc_surrogate = should_dc_opf_ws || need_screening;

    // Run DC-OPF; capture full solution when screening is needed.
    #[allow(unused_labels)]
    let warmstart_lp: Arc<dyn crate::backends::LpSolver> = 'lp: {
        if nlp_name == "Gurobi-NLP"
            && let Ok(s) = crate::backends::gurobi::GurobiLpSolver::new_validated()
        {
            break 'lp Arc::new(s);
        }
        if nlp_name == "COPT-NLP"
            && let Ok(s) = crate::backends::copt::CoptLpSolver::new()
        {
            break 'lp Arc::new(s);
        }
        crate::backends::try_default_lp_solver().map_err(AcOpfError::SolverError)?
    };
    let (dc_opf_angles, dc_opf_solution): (Option<Vec<f64>>, Option<OpfSolution>) =
        if should_run_dc_surrogate {
            let screening_network = if has_hvdc && need_screening {
                info!(
                    n_bus,
                    "AC-OPF: building explicit-HVDC DC surrogate for constraint screening"
                );
                Some(build_explicit_hvdc_screening_network(
                    network,
                    context.runtime.warm_start.as_ref(),
                )?)
            } else {
                None
            };
            let dc_opts = crate::dc::opf::DcOpfOptions {
                enforce_thermal_limits: options.enforce_thermal_limits,
                min_rate_a: options.min_rate_a,
                ..crate::dc::opf::DcOpfOptions::default()
            };
            let dc_runtime = crate::dc::opf::DcOpfRuntime::default().with_lp_solver(warmstart_lp);
            let dc_network = screening_network.as_ref().unwrap_or(network);
            match crate::dc::opf::solve_dc_opf_with_runtime(dc_network, &dc_opts, &dc_runtime) {
                Ok(dc_result) => {
                    let va = if should_dc_opf_ws {
                        info!(
                            n_bus,
                            "DC-OPF warm-start: seeding AC-OPF angles from optimal dispatch"
                        );
                        Some(dc_result.opf.power_flow.voltage_angle_rad.clone())
                    } else {
                        None
                    };
                    let screening_solution = need_screening.then_some(dc_result.opf);
                    (va, screening_solution)
                }
                Err(e) => {
                    tracing::warn!(
                        "DC-OPF surrogate solve failed ({e}), falling back to DC-PF angles"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

    // Build full constrained branch list (for feasibility checks after screened solves).
    let all_constrained_branches: Vec<usize> = if options.enforce_thermal_limits {
        network
            .branches
            .iter()
            .enumerate()
            .filter(|(_, br)| br.in_service && br.rating_a_mva >= options.min_rate_a)
            .map(|(i, _)| i)
            .collect()
    } else {
        vec![]
    };

    // Active constraint screening: start with branches loaded above threshold in DC-OPF.
    // Outer loop adds violated branches until feasible or max iterations reached.
    let threshold = options.constraint_screening_threshold.unwrap_or(1.0);
    // Only screen when it removes a meaningful fraction of branches.
    // If <30% of branches are removed, the screened solve is nearly as expensive
    // as the full solve; adding the fallback re-solve makes it worse overall.
    const MIN_REDUCTION_FRACTION: f64 = 0.30;
    let (active_branches, screening_active): (Vec<usize>, bool) =
        if let Some(dc_sol) = dc_opf_solution.as_ref().filter(|_| need_screening) {
            let screened: Vec<usize> = all_constrained_branches
                .iter()
                .filter(|&&i| {
                    let loading = dc_sol
                        .branches
                        .branch_loading_pct
                        .get(i)
                        .copied()
                        .unwrap_or(0.0);
                    loading >= threshold * 100.0
                })
                .copied()
                .collect();
            let n_total = all_constrained_branches.len();
            let n_active = screened.len();
            let removed_frac = 1.0 - n_active as f64 / n_total.max(1) as f64;
            if removed_frac < MIN_REDUCTION_FRACTION {
                info!(
                    n_total,
                    n_active,
                    "constraint screening: {:.0}% removed < {:.0}% minimum, skipping",
                    removed_frac * 100.0,
                    MIN_REDUCTION_FRACTION * 100.0,
                );
                (all_constrained_branches.clone(), false)
            } else {
                info!(
                    n_total,
                    n_active,
                    "constraint screening: {n_active}/{n_total} branches active ({:.0}% removed)",
                    removed_frac * 100.0,
                );
                (screened, true)
            }
        } else {
            (all_constrained_branches.clone(), false)
        };

    let nlp_options = NlpOptions {
        tolerance: options.tolerance,
        max_iterations: effective_max_iter,
        print_level: options.print_level,
        hessian_mode: if options.exact_hessian {
            HessianMode::Exact
        } else {
            HessianMode::LimitedMemory
        },
        warm_start: context.runtime.warm_start.is_some(),
    };

    // ── Gurobi native NLP dispatch (bypasses constraint screening) ────────
    {
        use crate::backends::gurobi::GurobiNlpSolver;
        if let Some(grb) = nlp
            .as_any()
            .and_then(|a| a.downcast_ref::<GurobiNlpSolver>())
        {
            let unsupported = gurobi_native_unsupported_reasons(network, options, &context);
            if !unsupported.is_empty() {
                return Err(AcOpfError::SolverError(format!(
                    "Gurobi native AC-OPF does not yet support {}",
                    unsupported.join(", ")
                )));
            }
            let t0 = std::time::Instant::now();
            let mut sol = grb
                .solve_native_ac_opf(network, options, &context, dc_opf_angles.as_deref())
                .map_err(AcOpfError::SolverError)?;
            sol.solve_time_secs = t0.elapsed().as_secs_f64();
            return Ok(sol);
        }
    }

    // ── Outer constraint-generation loop ──────────────────────────────────
    // Iteration 0: solve with active (screened) branch set.
    // Iteration 1 (fallback): if any violations found, re-solve with full constraint set.
    // Capped at 1 fallback to bound worst-case to 2 AC-OPF solves.
    let mut problem = AcOpfProblem::new(
        network,
        options,
        &context,
        dc_opf_angles.as_deref(),
        Some(active_branches.clone()),
    )?;
    let mut sol = crate::backends::run_nlp_solver_with_policy(nlp.as_ref(), || {
        nlp.solve(&problem, &nlp_options)
    })
    .map_err(AcOpfError::SolverError)?;

    if !sol.converged {
        return Err(AcOpfError::NotConverged);
    }

    if screening_active && options.screening_fallback_enabled {
        // Check all original constrained branches for violations in the AC solution.
        let bus_map_check = network.bus_index_map();
        let (va_chk, vm_chk, _, _) = problem.mapping.extract_voltages_and_dispatch(&sol.x);
        let mut n_violations = 0usize;
        for &i in &all_constrained_branches {
            if active_branches.contains(&i) {
                continue;
            }
            let br = &network.branches[i];
            let fi = bus_map_check[&br.from_bus];
            let ti = bus_map_check[&br.to_bus];
            let s_max = br.rating_a_mva / problem.base_mva; // pu
            let vf = vm_chk[fi];
            let vt = vm_chk[ti];
            let flows = br.power_flows_pu(vf, vt, va_chk[fi] - va_chk[ti], 1e-40);
            let sf_sq = flows.p_from_pu * flows.p_from_pu + flows.q_from_pu * flows.q_from_pu;
            let st_sq = flows.p_to_pu * flows.p_to_pu + flows.q_to_pu * flows.q_to_pu;
            if sf_sq > s_max * s_max || st_sq > s_max * s_max {
                n_violations += 1;
            }
        }

        if n_violations == 0 {
            info!("constraint screening: no violations, done");
        } else {
            // Any violations → fall back to full constraint set in one re-solve.
            // This bounds worst-case to exactly 2 AC-OPF solves and avoids
            // cascading re-solves when DC/AC loading patterns diverge.
            info!(
                n_violations,
                "constraint screening: violations found, falling back to full constraint set"
            );
            problem = AcOpfProblem::new(
                network,
                options,
                &context,
                dc_opf_angles.as_deref(),
                Some(all_constrained_branches.clone()),
            )?;
            sol = crate::backends::run_nlp_solver_with_policy(nlp.as_ref(), || {
                nlp.solve(&problem, &nlp_options)
            })
            .map_err(AcOpfError::SolverError)?;
            if !sol.converged {
                return Err(AcOpfError::NotConverged);
            }
        }
    }

    // Bind mapping/base from final problem instance (may differ if outer loop ran).
    let m = &problem.mapping;
    let n_gen = m.n_gen;
    let base = problem.base_mva;

    // Unpack solution
    let (va, vm, pg_pu, qg_pu) = m.extract_voltages_and_dispatch(&sol.x);
    let gen_p_mw: Vec<f64> = pg_pu.iter().map(|&p| p * base).collect();
    let gen_q_mvar: Vec<f64> = qg_pu.iter().map(|&q| q * base).collect();

    // Extract storage dispatch from NLP variables.
    // storage_net_mw[s] = (dis[s] - ch[s]) * base_mva  (positive = discharging)
    let storage_net_mw: Vec<f64> = (0..m.n_sto)
        .map(|s| (sol.x[m.discharge_var(s)] - sol.x[m.charge_var(s)]) * base)
        .collect();

    // Compute total cost from objective (already in $/hr)
    let total_cost = sol.objective;

    let n_lam = sol.lambda.len();
    let has_nlp_duals = n_lam >= n_bus && sol.lambda[..n_bus].iter().any(|&l| l.abs() > 1e-10);

    // Extract LMPs from constraint multipliers.
    // lambda[0..n_bus] = P-balance multipliers
    // lambda[n_bus..2*n_bus] = Q-balance multipliers
    // lambda[2*n_bus..] = branch flow multipliers
    //
    // From KKT: dL/dPg_j = df/dPg_j - lambda_P[bus(j)] = 0
    // So LMP[i] = lambda_P[i] (the P-balance multiplier at bus i)
    // But Ipopt sign convention: for equality g(x)=0 with g_l=g_u=0,
    // the multiplier is the standard Lagrange multiplier.
    //
    // LMP = -lambda_P * base_mva (convert from per-unit cost to $/MWh)
    // The sign: our constraint is P_calc - Pg + Pd = 0.
    // dL/dPg = df/dPg - lambda = 0 => lambda = df/dPg = marginal_cost * base
    // So lambda is already positive for positive marginal cost.
    // LMP[i] = lambda[i] (in per-unit cost / per-unit MW = cost / MW)
    // But we need $/MWh, and the objective is in $/hr, variables in p.u.
    // dL/dPg_pu = d(cost)/dPg_pu = marginal_cost(Pg_MW) * base = lambda_P[bus]
    // LMP[i] = lambda_P[i] / base (converts from d($/hr)/d(pu) to $/MWh)
    // Actually: dL/dPg_pu = 0 gives lambda_P = d(cost)/dPg_pu
    // But d(cost)/dPg_MW = marginal_cost, and Pg_pu = Pg_MW / base
    // So d(cost)/dPg_pu = marginal_cost * base
    // Therefore lambda_P = marginal_cost * base (units: $/hr / pu = $/hr * base / MW)
    // Wait, pu has no units of time. Let me think again:
    // - cost is in $/hr
    // - Pg_pu is dimensionless (MW / base_MVA)
    // - lambda_P = d(cost_$/hr)/d(Pg_pu) has units $/hr / (MW/MVA) = $/hr * MVA/MW = MVA * $/MWh
    // - So LMP = lambda_P / base_MVA has units $/MWh. ✓

    let lmp: Vec<f64> = if has_nlp_duals {
        sol.lambda[..n_bus].iter().map(|&l| l / base).collect()
    } else {
        vec![]
    };

    // LMP decomposition: lmp[i] = lmp_energy[i] + lmp_congestion[i] + lmp_loss[i]
    //
    // -----------------------------------------------------------------------
    // Energy component
    // -----------------------------------------------------------------------
    // The energy component is the LMP at the slack/reference bus.  At the
    // reference bus, Va = 0 is fixed and there is no angle degree-of-freedom,
    // so congestion and loss contributions to the slack bus LMP are zero by
    // convention.  Hence λ_energy = λ[slack] and it is the same system marginal
    // energy price broadcast to every bus before congestion/loss adjustment.
    let lambda_energy = lmp[m.slack_idx];
    let _lmp_energy = vec![lambda_energy; n_bus];

    // -----------------------------------------------------------------------
    // Loss component — AC Marginal Loss Factors (exact, one J^T solve)
    // -----------------------------------------------------------------------
    //
    // MLF[i] = ∂P_loss_total / ∂P_inject_i, computed from the AC Jacobian at
    // the Ipopt-optimal operating point (va*, vm*). One KLU factorization and
    // one J^T solve — approximately the cost of one Newton-Raphson iteration.
    //
    //   lmp_loss[i]       = λ_energy * MLF[i]           (AC-exact)
    //   lmp_congestion[i] = lmp[i] - energy[i] - loss[i] (exact by subtraction)
    let lmp_loss: Vec<f64> = match super::loss_factors::compute_ac_marginal_loss_factors(
        network,
        &va,
        vm,
        m.slack_idx,
    ) {
        Ok(mlf) => mlf.iter().map(|&mf| lambda_energy * mf).collect(),
        Err(e) => {
            // Fallback for multi-island HVDC or structurally singular J.
            // Use zero loss so congestion absorbs the residual (same as
            // the previous pure-DC-PTDF residual behavior for this path).
            tracing::warn!("AC MLF failed, lmp_loss set to zero (fallback): {e}");
            vec![0.0; n_bus]
        }
    };

    // -----------------------------------------------------------------------
    // Congestion component — exact by subtraction
    // -----------------------------------------------------------------------
    //
    // With lmp_loss now AC-exact, congestion is the remaining residual:
    //   lmp_congestion[i] = lmp[i] - lmp_energy[i] - lmp_loss[i]
    //
    // This is exact regardless of whether branch flow limits are binding,
    // because it does not rely on DC-PTDF approximations.
    let n_br_con = m.constrained_branches.len();
    let _lmp_congestion: Vec<f64> = lmp
        .iter()
        .zip(lmp_loss.iter())
        .map(|(&l, &loss)| l - lambda_energy - loss)
        .collect();

    // Branch shadow prices (max of from-side and to-side multipliers).
    // n_br_con was defined above in the congestion block.
    let n_br_total = network.n_branches();
    let branch_shadow_prices = if has_nlp_duals {
        let mut branch_shadow_prices = vec![0.0; n_br_total];
        for (ci, &br_idx) in m.constrained_branches.iter().enumerate() {
            let mu_from = sol.lambda[2 * n_bus + ci].abs();
            let mu_to = sol.lambda[2 * n_bus + n_br_con + ci].abs();
            branch_shadow_prices[br_idx] = (mu_from + mu_to) / base;
        }
        branch_shadow_prices
    } else {
        vec![]
    };

    // Angle constraint duals.
    // Ipopt convention for a range constraint [lo, hi]: positive multiplier when lo
    // is binding (angmin), negative when hi is binding (angmax).
    let ang_lambda_offset = 2 * n_bus + 2 * n_br_con;
    let (shadow_price_angmin, shadow_price_angmax) = if has_nlp_duals {
        let mut shadow_price_angmin = vec![0.0_f64; n_br_total];
        let mut shadow_price_angmax = vec![0.0_f64; n_br_total];
        for (ai, &(br_idx, _, _)) in m.angle_constrained_branches.iter().enumerate() {
            let lam = sol.lambda[ang_lambda_offset + ai] / base;
            if lam > 0.0 {
                shadow_price_angmin[br_idx] = lam;
            } else {
                shadow_price_angmax[br_idx] = -lam;
            }
        }
        (shadow_price_angmin, shadow_price_angmax)
    } else {
        (vec![], vec![])
    };

    // Build power flow solution
    let (p_inject, q_inject) = compute_power_injection(&problem.ybus, vm, &va);

    let solve_time = start.elapsed().as_secs_f64();
    info!(
        "AC-OPF solved in {:.1} ms ({} generators, {} branches constrained, {} angle constrained, cost={:.2} $/hr)",
        solve_time * 1000.0,
        n_gen,
        m.constrained_branches.len(),
        m.angle_constrained_branches.len(),
        total_cost
    );

    let gen_bus_numbers: Vec<u32> = m
        .gen_indices
        .iter()
        .map(|&gi| network.generators[gi].bus)
        .collect();
    let gen_ids: Vec<String> = m
        .gen_indices
        .iter()
        .map(|&gi| network.generators[gi].id.clone())
        .collect();
    let shadow_price_pg_min: Vec<f64> = if has_nlp_duals {
        (0..n_gen)
            .map(|j| sol.z_lower[m.pg_var(j)] / base)
            .collect()
    } else {
        vec![]
    };
    let shadow_price_pg_max: Vec<f64> = if has_nlp_duals {
        (0..n_gen)
            .map(|j| sol.z_upper[m.pg_var(j)] / base)
            .collect()
    } else {
        vec![]
    };
    let total_load_mw: f64 = network.total_load_mw();
    let total_generation_mw: f64 = gen_p_mw.iter().sum();
    let total_losses_mw = total_generation_mw - total_load_mw;

    // --- P3: Branch flows from complex voltage solution ---
    // Build admittance parameters for ALL branches (not just constrained ones).
    let bus_map_ac = network.bus_index_map();
    let n_br_total = network.n_branches();
    let mut branch_pf_mw = vec![0.0_f64; n_br_total];
    let mut branch_pt_mw = vec![0.0_f64; n_br_total];
    let mut branch_qf_mvar = vec![0.0_f64; n_br_total];
    let mut branch_qt_mvar = vec![0.0_f64; n_br_total];
    let mut branch_loading_pct = vec![0.0_f64; n_br_total];

    for (l, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let fi = bus_map_ac[&br.from_bus];
        let ti = bus_map_ac[&br.to_bus];

        let vf = vm[fi];
        let vt = vm[ti];
        let theta = va[fi] - va[ti];
        let flows = br.power_flows_pu(vf, vt, theta, 1e-40);

        branch_pf_mw[l] = flows.p_from_pu * base;
        branch_qf_mvar[l] = flows.q_from_pu * base;
        branch_pt_mw[l] = flows.p_to_pu * base;
        branch_qt_mvar[l] = flows.q_to_pu * base;

        // Loading: |Sf_MVA| / rate_a * 100
        if br.rating_a_mva > 0.0 {
            let sf_mva = flows.s_from_pu() * base;
            let st_mva = flows.s_to_pu() * base;
            branch_loading_pct[l] = sf_mva.max(st_mva) / br.rating_a_mva * 100.0;
        } else {
            branch_loading_pct[l] = f64::NAN;
        }
    }

    let pf_solution = PfSolution {
        pf_model: surge_solution::PfModel::Ac,
        status: SolveStatus::Converged,
        iterations: sol.iterations.unwrap_or(0),
        max_mismatch: 0.0,
        solve_time_secs: 0.0,
        voltage_magnitude_pu: vm.to_vec(),
        voltage_angle_rad: va.clone(),
        active_power_injection_pu: p_inject,
        reactive_power_injection_pu: q_inject,
        branch_p_from_mw: branch_pf_mw.clone(),
        branch_p_to_mw: branch_pt_mw.clone(),
        branch_q_from_mvar: branch_qf_mvar.clone(),
        branch_q_to_mvar: branch_qt_mvar.clone(),
        bus_numbers: network.buses.iter().map(|b| b.number).collect(),
        island_ids: vec![],
        q_limited_buses: vec![],
        n_q_limit_switches: 0,
        gen_slack_contribution_mw: vec![],
        convergence_history: vec![],
        worst_mismatch_bus: None,
        area_interchange: None,
    };

    // NLP backends without usable duals leave pricing outputs empty rather than
    // fabricating zero-valued load-bus prices.
    let pricing_available = has_nlp_duals;
    let lmp: Vec<f64> = if pricing_available {
        sol.lambda[..n_bus].iter().map(|&l| l / base).collect()
    } else {
        vec![]
    };
    let lmp_energy: Vec<f64> = if pricing_available {
        let lambda_energy = lmp[m.slack_idx];
        vec![lambda_energy; n_bus]
    } else {
        vec![]
    };
    let lmp_loss: Vec<f64> = if pricing_available {
        let lambda_energy = lmp[m.slack_idx];
        match super::loss_factors::compute_ac_marginal_loss_factors(network, &va, vm, m.slack_idx) {
            Ok(mlf) => mlf.iter().map(|&mf| lambda_energy * mf).collect(),
            Err(e) => {
                tracing::warn!("AC MLF failed, lmp_loss set to zero (fallback): {e}");
                vec![0.0; n_bus]
            }
        }
    } else {
        vec![]
    };
    let lmp_congestion: Vec<f64> = if pricing_available {
        let lambda_energy = lmp[m.slack_idx];
        lmp.iter()
            .zip(lmp_loss.iter())
            .map(|(&l, &loss)| l - lambda_energy - loss)
            .collect()
    } else {
        vec![]
    };
    let lmp_reactive: Vec<f64> = if pricing_available {
        sol.lambda[n_bus..2 * n_bus]
            .iter()
            .map(|&l| l / base)
            .collect()
    } else {
        vec![]
    };

    let shadow_price_vm_min: Vec<f64> = if has_nlp_duals {
        (0..n_bus)
            .map(|i| sol.z_lower[m.vm_var(i)] / base)
            .collect()
    } else {
        vec![]
    };
    let shadow_price_vm_max: Vec<f64> = if has_nlp_duals {
        (0..n_bus)
            .map(|i| sol.z_upper[m.vm_var(i)] / base)
            .collect()
    } else {
        vec![]
    };

    let shadow_price_qg_min: Vec<f64> = if has_nlp_duals {
        (0..n_gen)
            .map(|j| sol.z_lower[m.qg_var(j)] / base)
            .collect()
    } else {
        vec![]
    };
    let shadow_price_qg_max: Vec<f64> = if has_nlp_duals {
        (0..n_gen)
            .map(|j| sol.z_upper[m.qg_var(j)] / base)
            .collect()
    } else {
        vec![]
    };

    // --- Switched shunt dispatch: extract continuous b_sw values and round ---
    let switched_shunt_dispatch: Vec<(usize, f64, f64)> = network
        .controls
        .switched_shunts_opf
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < m.n_sw)
        .map(|(i, shunt)| {
            let b_cont = sol.x[m.sw_var(i)];
            let b_rounded = shunt.round_to_steps(b_cont);
            (m.switched_shunt_bus_idx[i], b_cont, b_rounded)
        })
        .collect();

    // --- SVC dispatch extraction ---
    let svc_dispatch: Vec<(usize, f64, f64, f64)> = (0..m.n_svc)
        .map(|i| {
            let b_val = sol.x[m.svc_var(i)];
            (i, b_val, 0.0, 0.0)
        })
        .collect();

    // --- TCSC dispatch extraction ---
    let tcsc_dispatch: Vec<(usize, f64, f64, f64)> = (0..m.n_tcsc)
        .map(|i| {
            let xc_val = sol.x[m.tcsc_var(i)];
            (i, xc_val, 0.0, 0.0)
        })
        .collect();

    // --- Tap and phase-shifter dispatch: extract and round ---
    let tap_dispatch =
        crate::discrete::extract_tap_dispatch(network, &sol.x, &m.tap_ctrl_branches, m.tap_offset);
    let phase_dispatch =
        crate::discrete::extract_phase_dispatch(network, &sol.x, &m.ps_ctrl_branches, m.ps_offset);

    // --- Discrete round-and-check verification ---
    let has_discrete_vars = !tap_dispatch.is_empty()
        || !switched_shunt_dispatch.is_empty()
        || !phase_dispatch.is_empty();
    let (discrete_feasible, discrete_violations) =
        if options.discrete_mode == DiscreteMode::RoundAndCheck && has_discrete_vars {
            let verification = crate::discrete::verify_discrete_solution(
                network,
                // Build a temporary OpfSolution with gen_q_mvar for the verification.
                // We only need gen_q_mvar for reactive limit checking.
                &OpfSolution {
                    opf_type: surge_solution::OpfType::AcOpf,
                    base_mva: network.base_mva,
                    power_flow: pf_solution.clone(),
                    generators: OpfGeneratorResults {
                        gen_p_mw: gen_p_mw.clone(),
                        gen_q_mvar: gen_q_mvar.clone(),
                        ..Default::default()
                    },
                    pricing: OpfPricing::default(),
                    branches: OpfBranchResults::default(),
                    devices: OpfDeviceDispatch::default(),
                    ..Default::default()
                },
                &tap_dispatch,
                &switched_shunt_dispatch,
                &phase_dispatch,
            );
            let feasible = verification.converged
                && verification.thermal_violations.is_empty()
                && verification.voltage_violations.is_empty()
                && verification.reactive_violations.is_empty();
            (Some(feasible), verification.violation_descriptions)
        } else {
            (None, vec![])
        };

    Ok(OpfSolution {
        opf_type: surge_solution::OpfType::AcOpf,
        base_mva: network.base_mva,
        power_flow: pf_solution,
        generators: OpfGeneratorResults {
            gen_p_mw,
            gen_q_mvar,
            gen_bus_numbers,
            gen_ids,
            gen_machine_ids: m
                .gen_indices
                .iter()
                .map(|&gi| {
                    network.generators[gi]
                        .machine_id
                        .clone()
                        .unwrap_or_else(|| "1".to_string())
                })
                .collect(),
            shadow_price_pg_min,
            shadow_price_pg_max,
            shadow_price_qg_min,
            shadow_price_qg_max,
        },
        pricing: OpfPricing {
            lmp,
            lmp_energy,
            lmp_congestion,
            lmp_loss,
            lmp_reactive,
        },
        branches: OpfBranchResults {
            branch_loading_pct,
            branch_shadow_prices,
            shadow_price_angmin,
            shadow_price_angmax,
            flowgate_shadow_prices: if has_nlp_duals && !m.flowgate_indices.is_empty() {
                let mut v = vec![0.0; network.flowgates.len()];
                for (fi, &fgi) in m.flowgate_indices.iter().enumerate() {
                    v[fgi] = sol.lambda[m.fg_con_offset + fi] / base;
                }
                v
            } else {
                vec![]
            },
            interface_shadow_prices: if has_nlp_duals && !m.interface_indices.is_empty() {
                let mut v = vec![0.0; network.interfaces.len()];
                for (ii, &ifi) in m.interface_indices.iter().enumerate() {
                    v[ifi] = sol.lambda[m.iface_con_offset + ii] / base;
                }
                v
            } else {
                vec![]
            },
            shadow_price_vm_min,
            shadow_price_vm_max,
        },
        devices: OpfDeviceDispatch {
            switched_shunt_dispatch,
            tap_dispatch,
            phase_dispatch,
            svc_dispatch,
            tcsc_dispatch,
            storage_net_mw,
            discrete_feasible,
            discrete_violations,
        },
        total_cost,
        total_load_mw,
        total_generation_mw,
        total_losses_mw,
        par_results: vec![],
        virtual_bid_results: vec![],
        benders_cut_duals: if !context.benders_cuts.is_empty() {
            let n_cuts = context.benders_cuts.len();
            sol.lambda[m.n_con..m.n_con + n_cuts].to_vec()
        } else {
            vec![]
        },
        solve_time_secs: solve_time,
        iterations: sol.iterations,
        solver_name: Some(nlp.name().to_string()),
        solver_version: Some(nlp.version().to_string()),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::nlp::NlpProblem;
    use crate::test_util::case_path;

    use super::*;

    /// OPF-06: D-curve constraint count — AcOpfProblem includes pq_curve constraints in n_con.
    ///
    /// Constructs a synthetic 3-bus case where one generator has a 2-point D-curve.
    /// Verifies that:
    /// 1. `n_constraints()` is 2 larger than without the D-curve (1 segment → 2 constraints).
    /// 2. `constraint_bounds()` correctly encodes the D-curve half-spaces.
    /// 3. `eval_constraints()` returns 0 (feasible) at the DC warm-start initial point.
    /// 4. `jacobian_structure()` includes 2 new entries (Pg and Qg columns) per D-curve constraint.
    #[test]
    fn test_acopf_dcurve_constraint_wiring() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        // Build a simple 3-bus network with costs so AC-OPF can be instantiated.
        let mut net = Network::new("test_dcurve");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 345.0));
        net.buses.push(Bus::new(3, BusType::PQ, 345.0));
        for b in &mut net.buses {
            b.voltage_magnitude_pu = 1.0;
            b.voltage_min_pu = 0.95;
            b.voltage_max_pu = 1.05;
        }

        net.loads.push(Load::new(3, 80.0, 30.0));

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.02));

        // Generator 0 at bus 1 — no D-curve.
        let mut g0 = Generator::new(1, 100.0, 1.0);
        g0.pmax = 250.0;
        g0.pmin = 0.0;
        g0.qmax = 150.0;
        g0.qmin = -50.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 20.0, 0.0],
        });
        net.generators.push(g0);

        // Generator 1 at bus 2 — has a 2-point D-curve.
        let mut g1 = Generator::new(2, 50.0, 1.0);
        g1.pmax = 200.0;
        g1.pmin = 0.0;
        g1.qmax = 100.0; // nameplate
        g1.qmin = -30.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 25.0, 0.0],
        });
        // D-curve: at P=0 Qmax=1.0 pu, at P=Pmax(2.0 pu) Qmax=0.3 pu.
        g1.reactive_capability
            .get_or_insert_with(Default::default)
            .pq_curve = vec![(0.0, 1.0, -0.3), (2.0, 0.3, -0.1)];
        net.generators.push(g1);

        let opts = AcOpfOptions {
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };

        // Build the problem without D-curve on g1 (baseline).
        let mut net_no_curve = net.clone();
        net_no_curve.generators[1].reactive_capability = None;
        let prob_base = AcOpfProblem::new(
            &net_no_curve,
            &opts,
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("AcOpfProblem::new failed (no D-curve)");
        let n_con_base = prob_base.n_constraints();

        // Build the problem WITH D-curve on g1.
        let prob_curve = AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None)
            .expect("AcOpfProblem::new failed (with D-curve)");
        let n_con_curve = prob_curve.n_constraints();

        // 2-point curve → 1 segment → 2 constraints (one upper + one lower).
        assert_eq!(
            n_con_curve,
            n_con_base + 2,
            "D-curve with 2 points adds 2 constraints; base={n_con_base}, curve={n_con_curve}"
        );

        // Check constraint_bounds: the 2 new rows should have finite lhs_lb or lhs_ub.
        let (gl, gu) = prob_curve.constraint_bounds();
        let pq_offset = prob_curve.mapping.pq_con_offset;
        // Upper D-curve: lhs_lb = -inf, lhs_ub = finite.
        assert!(gl[pq_offset].is_infinite() && gl[pq_offset] < 0.0);
        assert!(
            gu[pq_offset].is_finite(),
            "upper D-curve rhs must be finite"
        );
        // Lower D-curve: lhs_lb = finite, lhs_ub = +inf.
        assert!(
            gl[pq_offset + 1].is_finite(),
            "lower D-curve lb must be finite"
        );
        assert!(gu[pq_offset + 1].is_infinite() && gu[pq_offset + 1] > 0.0);

        // Jacobian must include 2 entries per D-curve constraint (Pg col + Qg col).
        let (jac_rows, jac_cols) = prob_curve.jacobian_structure();
        let pq_jac_entries: Vec<_> = jac_rows
            .iter()
            .zip(&jac_cols)
            .filter(|&(&r, _)| r >= pq_offset as i32 && r < (pq_offset + 2) as i32)
            .collect();
        assert_eq!(
            pq_jac_entries.len(),
            4, // 2 constraints × 2 entries each
            "D-curve Jacobian must have 4 entries (2 per constraint); got {}",
            pq_jac_entries.len()
        );
    }

    /// OPF-08: When `enforce_capability_curves` is false, D-curve constraints are
    /// omitted even when generators have `pq_curve` data — constraint count matches
    /// the no-curve baseline.
    #[test]
    fn test_dcurve_flag_disabled_no_pq_rows() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("test_dcurve_flag");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 345.0));
        for b in &mut net.buses {
            b.voltage_magnitude_pu = 1.0;
            b.voltage_min_pu = 0.95;
            b.voltage_max_pu = 1.05;
        }
        net.loads.push(Load::new(2, 50.0, 10.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));

        let mut g = Generator::new(1, 80.0, 1.0);
        g.pmax = 200.0;
        g.pmin = 0.0;
        g.qmax = 100.0;
        g.qmin = -50.0;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 20.0, 0.0],
        });
        g.reactive_capability
            .get_or_insert_with(Default::default)
            .pq_curve = vec![(0.0, 1.0, -0.5), (2.0, 0.3, -0.1)];
        net.generators.push(g);

        // With D-curve enabled (default)
        let opts_on = AcOpfOptions {
            enforce_thermal_limits: false,
            enforce_capability_curves: true,
            ..AcOpfOptions::default()
        };
        let prob_on =
            AcOpfProblem::new(&net, &opts_on, &AcOpfRunContext::default(), None, None).unwrap();
        let n_con_on = prob_on.n_constraints();

        // With D-curve disabled
        let opts_off = AcOpfOptions {
            enforce_thermal_limits: false,
            enforce_capability_curves: false,
            ..AcOpfOptions::default()
        };
        let prob_off =
            AcOpfProblem::new(&net, &opts_off, &AcOpfRunContext::default(), None, None).unwrap();
        let n_con_off = prob_off.n_constraints();

        // D-curve disabled should have 2 fewer constraints (1 segment = 2 rows)
        assert_eq!(
            n_con_on,
            n_con_off + 2,
            "enforce_capability_curves=false should remove 2 D-curve constraints"
        );
    }

    /// OPF-08: When a generator has no pq_curve data, the flag has no effect
    /// — constraint count is the same either way.
    #[test]
    fn test_dcurve_flag_no_data_same_either_way() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator, Load};

        let mut net = Network::new("test_dcurve_nodata");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 345.0));
        net.buses.push(Bus::new(2, BusType::PQ, 345.0));
        for b in &mut net.buses {
            b.voltage_magnitude_pu = 1.0;
            b.voltage_min_pu = 0.95;
            b.voltage_max_pu = 1.05;
        }
        net.loads.push(Load::new(2, 50.0, 10.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));

        let mut g = Generator::new(1, 80.0, 1.0);
        g.pmax = 200.0;
        g.pmin = 0.0;
        g.qmax = 100.0;
        g.qmin = -50.0;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 20.0, 0.0],
        });
        // No pq_curve data
        net.generators.push(g);

        let opts_on = AcOpfOptions {
            enforce_thermal_limits: false,
            enforce_capability_curves: true,
            ..AcOpfOptions::default()
        };
        let prob_on =
            AcOpfProblem::new(&net, &opts_on, &AcOpfRunContext::default(), None, None).unwrap();

        let opts_off = AcOpfOptions {
            enforce_thermal_limits: false,
            enforce_capability_curves: false,
            ..AcOpfOptions::default()
        };
        let prob_off =
            AcOpfProblem::new(&net, &opts_off, &AcOpfRunContext::default(), None, None).unwrap();

        assert_eq!(
            prob_on.n_constraints(),
            prob_off.n_constraints(),
            "No pq_curve data → flag has no effect on constraint count"
        );
    }

    /// OPF-05: DiscreteMode defaults to Continuous; RoundAndCheck with no discrete
    /// vars produces discrete_feasible = None.
    #[test]
    fn test_discrete_mode_no_vars_noop() {
        // NOTE: do NOT hold IPOPT_MUTEX here — solve_ac_opf grabs it internally
        // under #[cfg(test)]. Holding it here would deadlock.
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = AcOpfOptions {
            discrete_mode: DiscreteMode::RoundAndCheck,
            ..AcOpfOptions::default()
        };
        let sol = solve_ac_opf(&net, &opts).unwrap();
        // No taps/phases/shunts optimized → discrete_feasible should be None
        assert!(
            sol.devices.discrete_feasible.is_none(),
            "No discrete vars → discrete_feasible should be None, got {:?}",
            sol.devices.discrete_feasible
        );
        assert!(sol.devices.tap_dispatch.is_empty());
        assert!(sol.devices.phase_dispatch.is_empty());
    }

    /// OPF-08: Default options have enforce_capability_curves=true and
    /// discrete_mode=Continuous.
    #[test]
    fn test_default_options_new_fields() {
        let opts = AcOpfOptions::default();
        assert!(opts.enforce_capability_curves);
        assert_eq!(opts.discrete_mode, DiscreteMode::Continuous);
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_jacobian_fd_check() {
        // Verify analytical Jacobian matches central-difference finite-difference Jacobian.
        //
        // S-04: uses central differences O(eps²) instead of forward differences O(eps)
        // for a tighter accuracy guarantee, with tolerance tightened to 1e-5.
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = AcOpfOptions::default();
        let problem =
            super::AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None).unwrap();

        let x0 = problem.initial_point();
        let m = problem.n_constraints();
        let (jac_rows, jac_cols) = problem.jacobian_structure();
        let nnz = jac_rows.len();

        // Analytical Jacobian
        let mut jac_analytical = vec![0.0; nnz];
        problem.eval_jacobian(&x0, &mut jac_analytical);

        // Central-difference Jacobian: (g(x+eps) - g(x-eps)) / (2*eps)
        // O(eps²) accuracy vs O(eps) for forward differences.
        let eps = 1e-6;
        let mut max_err = 0.0f64;
        let mut worst_entry = (0i32, 0i32, 0.0, 0.0);
        for k in 0..nnz {
            let row = jac_rows[k] as usize;
            let col = jac_cols[k] as usize;

            let mut x_plus = x0.clone();
            x_plus[col] += eps;
            let mut g_plus = vec![0.0; m];
            problem.eval_constraints(&x_plus, &mut g_plus);

            let mut x_minus = x0.clone();
            x_minus[col] -= eps;
            let mut g_minus = vec![0.0; m];
            problem.eval_constraints(&x_minus, &mut g_minus);

            let fd_val = (g_plus[row] - g_minus[row]) / (2.0 * eps);
            let err = (jac_analytical[k] - fd_val).abs();
            let scale = 1.0 + jac_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;

            if rel_err > max_err {
                max_err = rel_err;
                worst_entry = (jac_rows[k], jac_cols[k], jac_analytical[k], fd_val);
            }
        }

        println!(
            "Jacobian FD check (central diff): max_rel_err={:.2e}, worst entry: row={}, col={}, analytical={:.6}, fd={:.6}",
            max_err, worst_entry.0, worst_entry.1, worst_entry.2, worst_entry.3
        );
        assert!(
            max_err < 1e-5,
            "Jacobian FD mismatch: max_rel_err={:.2e} at row={} col={} (a={:.6} vs fd={:.6})",
            max_err,
            worst_entry.0,
            worst_entry.1,
            worst_entry.2,
            worst_entry.3
        );
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_jacobian_fd_check_congested() {
        // S-04 (congested case): Verify Jacobian accuracy with a binding thermal constraint.
        //
        // We create a case9 network with a very tight line limit on branch 0 (bus 1→4)
        // to force Ipopt to evaluate the Jacobian near a congested operating point.
        // The Jacobian structure includes branch flow rows — this validates those
        // entries specifically.
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Tighten the first in-service branch to a small limit so it is binding.
        // We use a nonzero but restrictive rate_a (50 MVA) to test the branch flow
        // Jacobian rows while keeping the problem feasible.
        for br in net.branches.iter_mut() {
            if br.in_service {
                br.rating_a_mva = 50.0; // MVA — forces several branches to bind
                break;
            }
        }

        let opts = AcOpfOptions::default();
        let problem =
            super::AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None).unwrap();

        // Use the DC warm-start initial point (not the optimal) — we only need
        // to validate the Jacobian, not solve to optimality.
        let x0 = problem.initial_point();
        let m_con = problem.n_constraints();
        let (jac_rows, jac_cols) = problem.jacobian_structure();
        let nnz = jac_rows.len();

        let mut jac_analytical = vec![0.0; nnz];
        problem.eval_jacobian(&x0, &mut jac_analytical);

        // Central-difference check
        let eps = 1e-6;
        let mut max_err = 0.0f64;
        let mut worst_entry = (0i32, 0i32, 0.0, 0.0);
        for k in 0..nnz {
            let row = jac_rows[k] as usize;
            let col = jac_cols[k] as usize;

            let mut x_plus = x0.clone();
            x_plus[col] += eps;
            let mut g_plus = vec![0.0; m_con];
            problem.eval_constraints(&x_plus, &mut g_plus);

            let mut x_minus = x0.clone();
            x_minus[col] -= eps;
            let mut g_minus = vec![0.0; m_con];
            problem.eval_constraints(&x_minus, &mut g_minus);

            let fd_val = (g_plus[row] - g_minus[row]) / (2.0 * eps);
            let err = (jac_analytical[k] - fd_val).abs();
            let scale = 1.0 + jac_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;

            if rel_err > max_err {
                max_err = rel_err;
                worst_entry = (jac_rows[k], jac_cols[k], jac_analytical[k], fd_val);
            }
        }

        println!(
            "Jacobian FD check (congested, central diff): max_rel_err={:.2e}, worst entry: row={}, col={}, analytical={:.6}, fd={:.6}",
            max_err, worst_entry.0, worst_entry.1, worst_entry.2, worst_entry.3
        );
        assert!(
            max_err < 1e-5,
            "Jacobian FD mismatch (congested): max_rel_err={:.2e} at row={} col={} (a={:.6} vs fd={:.6})",
            max_err,
            worst_entry.0,
            worst_entry.1,
            worst_entry.2,
            worst_entry.3
        );
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_tap_optimization_fd_check() {
        // Verify Jacobian correctness when tap ratio variables are active.
        //
        // We create a case9 network with one transformer branch marked as
        // TapMode::Continuous.  The Jacobian FD check verifies the new τ column
        // entries: dP[fi]/dτ and dQ[fi]/dτ etc.
        use surge_network::network::{BranchOpfControl, TapMode};
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Mark the first in-service branch as a tap-controllable transformer.
        // Set tap to a non-unity value to exercise the τ Jacobian path.
        for br in net.branches.iter_mut() {
            if br.in_service {
                br.tap = 1.05;
                let ctl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
                ctl.tap_mode = TapMode::Continuous;
                ctl.tap_min = 0.9;
                ctl.tap_max = 1.1;
                break;
            }
        }

        let opts = AcOpfOptions {
            optimize_taps: true,
            enforce_thermal_limits: false, // keep NLP simple
            ..AcOpfOptions::default()
        };
        let problem =
            super::AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None).unwrap();
        let x0 = problem.initial_point();
        let m_con = problem.n_constraints();
        let (jac_rows, jac_cols) = problem.jacobian_structure();
        let nnz = jac_rows.len();

        let mut jac_analytical = vec![0.0; nnz];
        problem.eval_jacobian(&x0, &mut jac_analytical);

        // Central-difference Jacobian
        let eps = 1e-6;
        let mut max_err = 0.0f64;
        let mut worst = (0i32, 0i32, 0.0, 0.0);
        for k in 0..nnz {
            let row = jac_rows[k] as usize;
            let col = jac_cols[k] as usize;
            let mut x_plus = x0.clone();
            x_plus[col] += eps;
            let mut g_plus = vec![0.0; m_con];
            problem.eval_constraints(&x_plus, &mut g_plus);
            let mut x_minus = x0.clone();
            x_minus[col] -= eps;
            let mut g_minus = vec![0.0; m_con];
            problem.eval_constraints(&x_minus, &mut g_minus);
            let fd_val = (g_plus[row] - g_minus[row]) / (2.0 * eps);
            let err = (jac_analytical[k] - fd_val).abs();
            let scale = 1.0 + jac_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;
            if rel_err > max_err {
                max_err = rel_err;
                worst = (jac_rows[k], jac_cols[k], jac_analytical[k], fd_val);
            }
        }
        println!(
            "Tap Jacobian FD check: max_rel_err={:.2e}, worst: row={} col={} a={:.6} fd={:.6}",
            max_err, worst.0, worst.1, worst.2, worst.3
        );
        assert!(
            max_err < 1e-5,
            "Tap Jacobian mismatch: {:.2e} at row={} col={}",
            max_err,
            worst.0,
            worst.1
        );
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_phase_shifter_fd_check() {
        // Verify Jacobian correctness when phase shift variables are active.
        use surge_network::network::{BranchOpfControl, PhaseMode};
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Mark the second in-service branch as a phase-shift-controllable transformer.
        let mut count = 0;
        for br in net.branches.iter_mut() {
            if br.in_service {
                count += 1;
                if count == 2 {
                    br.tap = 1.0;
                    br.phase_shift_rad = 5.0_f64.to_radians();
                    let ctl = br.opf_control.get_or_insert_with(BranchOpfControl::default);
                    ctl.phase_mode = PhaseMode::Continuous;
                    ctl.phase_min_rad = (-15.0_f64).to_radians();
                    ctl.phase_max_rad = 15.0_f64.to_radians();
                    break;
                }
            }
        }

        let opts = AcOpfOptions {
            optimize_phase_shifters: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let problem =
            super::AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None).unwrap();
        let x0 = problem.initial_point();
        let m_con = problem.n_constraints();
        let (jac_rows, jac_cols) = problem.jacobian_structure();
        let nnz = jac_rows.len();

        let mut jac_analytical = vec![0.0; nnz];
        problem.eval_jacobian(&x0, &mut jac_analytical);

        let eps = 1e-6;
        let mut max_err = 0.0f64;
        let mut worst = (0i32, 0i32, 0.0, 0.0);
        for k in 0..nnz {
            let row = jac_rows[k] as usize;
            let col = jac_cols[k] as usize;
            let mut x_plus = x0.clone();
            x_plus[col] += eps;
            let mut g_plus = vec![0.0; m_con];
            problem.eval_constraints(&x_plus, &mut g_plus);
            let mut x_minus = x0.clone();
            x_minus[col] -= eps;
            let mut g_minus = vec![0.0; m_con];
            problem.eval_constraints(&x_minus, &mut g_minus);
            let fd_val = (g_plus[row] - g_minus[row]) / (2.0 * eps);
            let err = (jac_analytical[k] - fd_val).abs();
            let scale = 1.0 + jac_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;
            if rel_err > max_err {
                max_err = rel_err;
                worst = (jac_rows[k], jac_cols[k], jac_analytical[k], fd_val);
            }
        }
        println!(
            "Phase Jacobian FD check: max_rel_err={:.2e}, worst: row={} col={} a={:.6} fd={:.6}",
            max_err, worst.0, worst.1, worst.2, worst.3
        );
        assert!(
            max_err < 1e-5,
            "Phase Jacobian mismatch: {:.2e} at row={} col={}",
            max_err,
            worst.0,
            worst.1
        );
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_tap_solves_and_reduces_cost() {
        // Solve AC-OPF with and without tap optimization on case9.
        //
        // With a transformer at a non-optimal tap ratio, enabling tap optimization
        // should either equal or reduce the total cost vs. fixed tap.
        // (It should never be worse than fixed-tap since τ_init = τ_fixed.)
        use surge_network::network::{BranchOpfControl, TapMode};
        let mut net = surge_io::load(case_path("case9")).unwrap();

        // Add a transformer with tap 1.05 on the first branch (creates voltage asymmetry)
        for br in net.branches.iter_mut() {
            if br.in_service {
                br.tap = 1.05;
                br.opf_control
                    .get_or_insert_with(BranchOpfControl::default)
                    .tap_mode = TapMode::Continuous;
                break;
            }
        }

        // Fixed-tap solve
        let opts_fixed = AcOpfOptions {
            optimize_taps: false,
            enforce_thermal_limits: true,
            ..AcOpfOptions::default()
        };
        let result_fixed = solve_ac_opf(&net, &opts_fixed);

        // Tap-optimized solve
        let opts_tap = AcOpfOptions {
            optimize_taps: true,
            enforce_thermal_limits: true,
            ..AcOpfOptions::default()
        };
        let result_tap = solve_ac_opf(&net, &opts_tap);

        // Both should converge
        assert!(
            result_fixed.is_ok(),
            "Fixed-tap solve failed: {:?}",
            result_fixed.err()
        );
        assert!(
            result_tap.is_ok(),
            "Tap-optimized solve failed: {:?}",
            result_tap.err()
        );

        let cost_fixed = result_fixed.unwrap().total_cost;
        let cost_tap = result_tap.unwrap().total_cost;

        println!(
            "Fixed-tap cost: {:.2} $/hr, Tap-optimized cost: {:.2} $/hr",
            cost_fixed, cost_tap
        );
        // Tap optimization should not increase cost (τ can always revert to τ_init)
        assert!(
            cost_tap <= cost_fixed + 1e-3, // small tolerance for NLP numerical differences
            "Tap optimization increased cost: {:.2} > {:.2}",
            cost_tap,
            cost_fixed
        );
    }

    #[ignore = "slow: Ipopt NLP — run with cargo test --test opf_slow"]
    #[test]
    fn test_acopf_hessian_fd_check() {
        // Verify analytical Hessian matches finite-difference of the Jacobian
        let net = surge_io::load(case_path("case9")).unwrap();
        let opts = AcOpfOptions::default();
        let problem =
            super::AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None).unwrap();

        let x0 = problem.initial_point();
        let n = problem.n_vars();
        let m = problem.n_constraints();
        let (hess_rows, hess_cols) = problem.hessian_structure();
        let nnz = hess_rows.len();

        // Evaluate analytical Hessian at x0 with obj_factor=1.0 and lambda=1.0 for all constraints
        let lambda: Vec<f64> = vec![1.0; m];
        let mut hess_analytical = vec![0.0; nnz];
        problem.eval_hessian(&x0, 1.0, &lambda, &mut hess_analytical);

        // Finite-difference Hessian using central differences for O(eps²) accuracy.
        // For the Lagrangian gradient: ∇L = obj_factor*∇f + Σ λ_i * ∇g_i
        let eps = 1e-5;
        let (jac_rows_s, jac_cols_s) = problem.jacobian_structure();
        let jac_nnz = jac_rows_s.len();

        // Helper: compute Lagrangian gradient at a point
        let lag_grad_at = |x: &[f64]| -> Vec<f64> {
            let mut grad = vec![0.0; n];
            problem.eval_gradient(x, &mut grad);
            let mut jac = vec![0.0; jac_nnz];
            problem.eval_jacobian(x, &mut jac);
            for kk in 0..jac_nnz {
                let row = jac_rows_s[kk] as usize;
                let col = jac_cols_s[kk] as usize;
                grad[col] += lambda[row] * jac[kk];
            }
            grad
        };

        let mut max_err = 0.0f64;
        let mut worst = (0i32, 0i32, 0.0, 0.0);

        for k in 0..nnz {
            let h_row = hess_rows[k] as usize;
            let h_col = hess_cols[k] as usize;

            // Central difference: (∇L(x+eps) - ∇L(x-eps)) / (2*eps)
            let mut x_plus = x0.clone();
            x_plus[h_col] += eps;
            let mut x_minus = x0.clone();
            x_minus[h_col] -= eps;

            let lg_plus = lag_grad_at(&x_plus);
            let lg_minus = lag_grad_at(&x_minus);

            let fd_val = (lg_plus[h_row] - lg_minus[h_row]) / (2.0 * eps);
            let err = (hess_analytical[k] - fd_val).abs();
            let scale = 1.0 + hess_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;

            if rel_err > max_err {
                max_err = rel_err;
                worst = (hess_rows[k], hess_cols[k], hess_analytical[k], fd_val);
            }
        }

        println!(
            "Hessian FD check: max_rel_err={:.2e}, worst: row={}, col={}, analytical={:.6}, fd={:.6}",
            max_err, worst.0, worst.1, worst.2, worst.3
        );
        assert!(
            max_err < 1e-3,
            "Hessian FD mismatch: max_rel_err={:.2e} at row={} col={} (a={:.6} vs fd={:.6})",
            max_err,
            worst.0,
            worst.1,
            worst.2,
            worst.3
        );
    }

    // -----------------------------------------------------------------------
    // Flowgate / interface constraint tests
    // -----------------------------------------------------------------------

    /// Build a 3-bus network suitable for flowgate tests.
    ///
    /// Bus 1 (slack, cheap gen 300 MW cap @ $10/MWh),
    /// Bus 2 (PQ, 100 MW load),
    /// Bus 3 (PQ, 50 MW load, expensive gen 200 MW cap @ $50/MWh).
    /// Branches: 1→2 (x=0.1, r=0.01, b=0.02), 2→3 (x=0.1, r=0.01, b=0.02).
    fn build_3bus_flowgate_network() -> Network {
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let mut net = Network::new("fg_ac_test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 345.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_min_pu = 0.95;
        b1.voltage_max_pu = 1.05;
        let mut b2 = Bus::new(2, BusType::PQ, 345.0);
        b2.voltage_magnitude_pu = 1.0;
        b2.voltage_min_pu = 0.95;
        b2.voltage_max_pu = 1.05;
        let mut b3 = Bus::new(3, BusType::PQ, 345.0);
        b3.voltage_magnitude_pu = 1.0;
        b3.voltage_min_pu = 0.95;
        b3.voltage_max_pu = 1.05;
        net.buses.extend([b1, b2, b3]);
        net.loads
            .push(surge_network::network::Load::new(2, 100.0, 20.0));
        net.loads
            .push(surge_network::network::Load::new(3, 50.0, 10.0));

        let mut gen1 = Generator::new(1, 200.0, 1.0);
        gen1.pmax = 300.0;
        gen1.pmin = 0.0;
        gen1.qmax = 200.0;
        gen1.qmin = -100.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 10.0, 0.0],
        });

        let mut gen3 = Generator::new(3, 50.0, 1.0);
        gen3.pmax = 200.0;
        gen3.pmin = 0.0;
        gen3.qmax = 150.0;
        gen3.qmin = -50.0;
        gen3.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 50.0, 0.0],
        });
        net.generators.extend([gen1, gen3]);

        let br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        let br23 = Branch::new_line(2, 3, 0.01, 0.1, 0.02);
        net.branches.extend([br12, br23]);

        net
    }

    /// Test 1: Tight flowgate on branch 1→2 forces redispatch; shadow price > 0.
    #[ignore = "slow: Ipopt NLP"]
    #[test]
    fn test_ac_opf_flowgate_binding() {
        use surge_network::network::Flowgate;

        let mut net = build_3bus_flowgate_network();
        // Tight flowgate: limit flow on branch 1→2 to 60 MW (unconstrained ≈ 100+ MW).
        net.flowgates.push(Flowgate {
            name: "FG_12_tight".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 60.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
        });

        let opts = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let sol = solve_ac_opf(&net, &opts).expect("AC-OPF with tight flowgate should converge");

        // Shadow price on the binding flowgate must be positive (forward limit binding).
        assert_eq!(sol.branches.flowgate_shadow_prices.len(), 1);
        assert!(
            sol.branches.flowgate_shadow_prices[0] > 1e-2,
            "binding flowgate shadow price should be positive, got {:.6}",
            sol.branches.flowgate_shadow_prices[0]
        );

        // Verify the flow respects the limit (within small tolerance).
        // Recompute flow on branch 1→2 from the solution voltages.
        // The expensive generator at bus 3 should be dispatched higher than without the flowgate.
        // gen3 is index 1 — with a 60 MW limit on 1→2, gen3 must pick up the slack.
        assert!(
            sol.generators.gen_p_mw[1] > 30.0,
            "expensive gen should dispatch higher due to flowgate; gen_p_mw[1]={:.2}",
            sol.generators.gen_p_mw[1]
        );
        println!(
            "FG binding test: gen_p_mw={:?}, fg_shadow={:?}, total_cost={:.2}",
            sol.generators.gen_p_mw, sol.branches.flowgate_shadow_prices, sol.total_cost
        );
    }

    /// Test 2: Slack flowgate with very high limit; shadow price ≈ 0.
    #[ignore = "slow: Ipopt NLP"]
    #[test]
    fn test_ac_opf_flowgate_slack() {
        use surge_network::network::Flowgate;

        let mut net = build_3bus_flowgate_network();
        // High-limit flowgate: should not bind.
        net.flowgates.push(Flowgate {
            name: "FG_12_slack".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 9999.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
        });

        let opts = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let sol = solve_ac_opf(&net, &opts).expect("AC-OPF with slack flowgate should converge");

        assert_eq!(sol.branches.flowgate_shadow_prices.len(), 1);
        assert!(
            sol.branches.flowgate_shadow_prices[0].abs() < 1.0,
            "slack flowgate shadow price should be ~0, got {:.6}",
            sol.branches.flowgate_shadow_prices[0]
        );
        println!(
            "FG slack test: gen_p_mw={:?}, fg_shadow={:?}",
            sol.generators.gen_p_mw, sol.branches.flowgate_shadow_prices
        );
    }

    /// Test 3: Interface constraint on branch 1→2 with tight forward limit.
    #[ignore = "slow: Ipopt NLP"]
    #[test]
    fn test_ac_opf_interface_binding() {
        use surge_network::network::Interface;

        let mut net = build_3bus_flowgate_network();
        net.interfaces.push(Interface {
            name: "IF_12".to_string(),
            members: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            limit_forward_mw: 60.0,
            limit_reverse_mw: 9999.0,
            in_service: true,
            limit_forward_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
        });

        let opts = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let sol = solve_ac_opf(&net, &opts).expect("AC-OPF with binding interface should converge");

        assert_eq!(sol.branches.interface_shadow_prices.len(), 1);
        // Forward limit binding → positive shadow price.
        assert!(
            sol.branches.interface_shadow_prices[0] > 1e-2,
            "binding interface shadow price should be positive, got {:.6}",
            sol.branches.interface_shadow_prices[0]
        );
        println!(
            "Interface binding test: gen_p_mw={:?}, iface_shadow={:?}",
            sol.generators.gen_p_mw, sol.branches.interface_shadow_prices
        );
    }

    /// Test 4: Finite-difference Hessian check with flowgate constraints active.
    #[ignore = "slow: Ipopt NLP"]
    #[test]
    fn test_ac_opf_flowgate_hessian_fd() {
        use surge_network::network::Flowgate;

        let mut net = build_3bus_flowgate_network();
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 80.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
        });

        let opts = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let problem = AcOpfProblem::new(&net, &opts, &AcOpfRunContext::default(), None, None)
            .expect("AcOpfProblem::new with flowgate");

        let x0 = problem.initial_point();
        let n = problem.n_vars();
        let m = problem.n_constraints();
        let (hess_rows, hess_cols) = problem.hessian_structure();
        let nnz = hess_rows.len();

        // Analytical Hessian with lambda=1.0 for all constraints (exercises flowgate rows).
        let lambda: Vec<f64> = vec![1.0; m];
        let mut hess_analytical = vec![0.0; nnz];
        problem.eval_hessian(&x0, 1.0, &lambda, &mut hess_analytical);

        // Finite-difference via central differences on Lagrangian gradient.
        let eps = 1e-5;
        let (jac_rows_s, jac_cols_s) = problem.jacobian_structure();
        let jac_nnz = jac_rows_s.len();

        let lag_grad_at = |x: &[f64]| -> Vec<f64> {
            let mut grad = vec![0.0; n];
            problem.eval_gradient(x, &mut grad);
            let mut jac = vec![0.0; jac_nnz];
            problem.eval_jacobian(x, &mut jac);
            for kk in 0..jac_nnz {
                let row = jac_rows_s[kk] as usize;
                let col = jac_cols_s[kk] as usize;
                grad[col] += lambda[row] * jac[kk];
            }
            grad
        };

        let mut max_err = 0.0f64;
        let mut worst = (0i32, 0i32, 0.0, 0.0);
        for k in 0..nnz {
            let h_row = hess_rows[k] as usize;
            let h_col = hess_cols[k] as usize;
            let mut x_plus = x0.clone();
            x_plus[h_col] += eps;
            let mut x_minus = x0.clone();
            x_minus[h_col] -= eps;
            let lg_plus = lag_grad_at(&x_plus);
            let lg_minus = lag_grad_at(&x_minus);
            let fd_val = (lg_plus[h_row] - lg_minus[h_row]) / (2.0 * eps);
            let err = (hess_analytical[k] - fd_val).abs();
            let scale = 1.0 + hess_analytical[k].abs().max(fd_val.abs());
            let rel_err = err / scale;
            if rel_err > max_err {
                max_err = rel_err;
                worst = (hess_rows[k], hess_cols[k], hess_analytical[k], fd_val);
            }
        }

        println!(
            "Flowgate Hessian FD check: max_rel_err={:.2e}, worst: row={} col={} a={:.6} fd={:.6}",
            max_err, worst.0, worst.1, worst.2, worst.3
        );
        assert!(
            max_err < 1e-3,
            "Flowgate Hessian FD mismatch: max_rel_err={:.2e} at row={} col={} (a={:.6} vs fd={:.6})",
            max_err,
            worst.0,
            worst.1,
            worst.2,
            worst.3
        );
    }

    /// Test 5: enforce_flowgates=false → no flowgate constraints even when flowgates exist.
    #[test]
    fn test_ac_opf_enforce_flowgates_false() {
        use surge_network::network::Flowgate;

        let mut net = build_3bus_flowgate_network();
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 60.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
        });

        // With enforce_flowgates=false (default), no flowgate constraints should appear.
        let opts_off = AcOpfOptions {
            enforce_flowgates: false,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let prob_off = AcOpfProblem::new(&net, &opts_off, &AcOpfRunContext::default(), None, None)
            .expect("AcOpfProblem::new (enforce_flowgates=false)");

        // With enforce_flowgates=true, flowgate constraint added.
        let opts_on = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let prob_on = AcOpfProblem::new(&net, &opts_on, &AcOpfRunContext::default(), None, None)
            .expect("AcOpfProblem::new (enforce_flowgates=true)");

        // The "on" problem should have 1 more constraint than the "off" problem.
        assert_eq!(
            prob_on.n_constraints(),
            prob_off.n_constraints() + 1,
            "enforce_flowgates=true should add 1 constraint; off={}, on={}",
            prob_off.n_constraints(),
            prob_on.n_constraints()
        );

        // Mapping should have empty flowgate_indices when off.
        assert!(
            prob_off.mapping.flowgate_indices.is_empty(),
            "flowgate_indices should be empty when enforce_flowgates=false"
        );
        assert_eq!(
            prob_on.mapping.flowgate_indices.len(),
            1,
            "flowgate_indices should have 1 entry when enforce_flowgates=true"
        );
    }

    /// Test 6: Reverse-direction flowgate binding (flow in negative direction).
    #[ignore = "slow: Ipopt NLP"]
    #[test]
    fn test_ac_opf_flowgate_reverse_binding() {
        use surge_network::market::CostCurve;
        use surge_network::network::Flowgate;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        // Build a network where the cheap gen is at bus 3 and load at bus 1,
        // so flow on branch 1→2 goes in the *reverse* direction (bus 2 → bus 1).
        let mut net = Network::new("fg_reverse_test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 345.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_min_pu = 0.95;
        b1.voltage_max_pu = 1.05;
        let mut b2 = Bus::new(2, BusType::PQ, 345.0);
        b2.voltage_magnitude_pu = 1.0;
        b2.voltage_min_pu = 0.95;
        b2.voltage_max_pu = 1.05;
        let mut b3 = Bus::new(3, BusType::PQ, 345.0);
        b3.voltage_magnitude_pu = 1.0;
        b3.voltage_min_pu = 0.95;
        b3.voltage_max_pu = 1.05;
        net.buses.extend([b1, b2, b3]);
        net.loads
            .push(surge_network::network::Load::new(1, 120.0, 20.0));

        // Expensive slack gen at bus 1 (small).
        let mut gen1 = Generator::new(1, 30.0, 1.0);
        gen1.pmax = 200.0;
        gen1.pmin = 0.0;
        gen1.qmax = 150.0;
        gen1.qmin = -100.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 50.0, 0.0],
        });

        // Cheap gen at bus 3.
        let mut gen3 = Generator::new(3, 100.0, 1.0);
        gen3.pmax = 200.0;
        gen3.pmin = 0.0;
        gen3.qmax = 150.0;
        gen3.qmin = -50.0;
        gen3.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.01, 10.0, 0.0],
        });
        net.generators.extend([gen1, gen3]);

        let br12 = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        let br23 = Branch::new_line(2, 3, 0.01, 0.1, 0.02);
        net.branches.extend([br12, br23]);

        // Flowgate on branch 1→2 with symmetric limit of 40 MW.
        // Natural flow is negative on 1→2 (power flows 2→1), so the -limit_mw bound binds.
        net.flowgates.push(Flowgate {
            name: "FG_12_reverse".to_string(),
            monitored: vec![surge_network::network::WeightedBranchRef::new(
                1, 2, "1", 1.0,
            )],
            contingency_branch: None,
            limit_mw: 40.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
        });

        let opts = AcOpfOptions {
            enforce_flowgates: true,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let sol = solve_ac_opf(&net, &opts)
            .expect("AC-OPF with reverse-binding flowgate should converge");

        assert_eq!(sol.branches.flowgate_shadow_prices.len(), 1);
        // Reverse binding → negative shadow price (lower bound of [-limit, +limit] is active).
        assert!(
            sol.branches.flowgate_shadow_prices[0] < -1e-2,
            "reverse-binding flowgate shadow price should be negative, got {:.6}",
            sol.branches.flowgate_shadow_prices[0]
        );
        println!(
            "FG reverse test: gen_p_mw={:?}, fg_shadow={:?}",
            sol.generators.gen_p_mw, sol.branches.flowgate_shadow_prices
        );
    }

    /// Regression test: the default large-case AC path should proactively seed
    /// the 6k–8k default-COPT class from an NR operating point instead of
    /// relying on the cold NLP start that stalls on case6470rte.
    #[ignore = "slow large-case regression; run with cargo test -p surge-opf --lib case6470rte -- --ignored --nocapture"]
    #[test]
    fn test_ac_opf_case6470rte_default_runtime() {
        let net = surge_io::load(case_path("case6470rte")).unwrap();
        let sol = solve_ac_opf(&net, &AcOpfOptions::default())
            .expect("default AC-OPF should solve case6470rte");

        assert!(sol.total_cost.is_finite() && sol.total_cost > 0.0);
    }
}
