// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Core Newton-Raphson solve logic: kernel, multi-island, adaptive startup.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::{PfModel, PfSolution, SolveStatus, compute_branch_power_flows};
use tracing::{debug, error, info, warn};

use super::nr_bus_setup::{
    apply_angle_reference, apply_generator_p_limit_demotions, apply_generator_voltage_setpoints,
    apply_remote_reg_types, build_participation_map, build_remote_reg_map, build_zip_bus_data,
    classify_indices, dc_angle_init, reclassify_dead_pv_buses,
};
use super::nr_kernel::{
    NrKernelOptions, NrState, NrWorkspace, PreparedNrModel, build_augmented_slack_data,
    run_nr_inner,
};
use super::nr_options::{AcPfError, AcPfOptions, StartupPolicy, WarmStart};
use super::nr_q_limits::{NrMeta, apply_per_gen_q_limits, build_gen_q_states, build_nr_meta};
use crate::control::discrete::{
    ResolvedSwitchedShunt, apply_oltc_steps, apply_par_steps, apply_shunt_steps,
};
use crate::matrix::mismatch::compute_power_injection;
use crate::matrix::ybus::build_ybus;
use crate::topology::islands::detect_islands;

pub(crate) fn validate_branch_endpoints(network: &Network) -> Result<(), AcPfError> {
    let bus_map = network.bus_index_map();
    for br in network.branches.iter().filter(|b| b.in_service) {
        if !bus_map.contains_key(&br.from_bus) {
            return Err(AcPfError::InvalidNetwork(format!(
                "branch ({} → {}) references unknown from_bus {}",
                br.from_bus, br.to_bus, br.from_bus
            )));
        }
        if !bus_map.contains_key(&br.to_bus) {
            return Err(AcPfError::InvalidNetwork(format!(
                "branch ({} → {}) references unknown to_bus {}",
                br.from_bus, br.to_bus, br.to_bus
            )));
        }
    }
    Ok(())
}

pub(crate) fn is_retryable_startup_error(err: &AcPfError) -> bool {
    matches!(
        err,
        AcPfError::NotConverged { .. } | AcPfError::NumericalFailure(_)
    )
}

pub(crate) fn make_startup_retry_options(
    options: &AcPfOptions,
    flat_start: bool,
    dc_warm_start: bool,
) -> AcPfOptions {
    let mut retry = options.clone();
    retry.flat_start = flat_start;
    retry.dc_warm_start = dc_warm_start;
    retry.warm_start = None;
    retry
}

pub(crate) fn nr_inner_stall_limit(options: &AcPfOptions) -> u32 {
    options.inner_stall_limit()
}

pub(crate) fn nr_inner_iteration_limit(options: &AcPfOptions) -> u32 {
    if options.startup_policy == StartupPolicy::Adaptive
        && options.flat_start
        && !options.dc_warm_start
        && options.warm_start.is_none()
    {
        options.max_iterations.min(8)
    } else {
        options.max_iterations
    }
}

fn resolve_switched_shunts(
    network: &Network,
    shunts: &[surge_network::network::SwitchedShunt],
) -> Result<Vec<ResolvedSwitchedShunt>, AcPfError> {
    let bus_map = network.bus_index_map();
    shunts
        .iter()
        .map(|shunt| {
            let bus_idx = *bus_map.get(&shunt.bus).ok_or_else(|| {
                AcPfError::InvalidNetwork(format!(
                    "switched shunt `{}` references unknown host bus {}",
                    shunt.id, shunt.bus
                ))
            })?;
            let bus_regulated_idx = *bus_map.get(&shunt.bus_regulated).ok_or_else(|| {
                AcPfError::InvalidNetwork(format!(
                    "switched shunt `{}` references unknown regulated bus {}",
                    shunt.id, shunt.bus_regulated
                ))
            })?;
            Ok(ResolvedSwitchedShunt {
                id: shunt.id.clone(),
                bus_idx,
                bus_regulated_idx,
                b_step: shunt.b_step,
                n_steps_cap: shunt.n_steps_cap,
                n_steps_react: shunt.n_steps_react,
                v_target: shunt.v_target,
                v_band: shunt.v_band,
                n_active_steps: shunt.n_active_steps,
            })
        })
        .collect()
}

pub(crate) fn attempt_adaptive_startup(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    let primary = make_startup_retry_options(options, options.flat_start, options.dc_warm_start);
    let mut last_retryable_err = match solve_ac_pf_kernel_core(network, &primary) {
        Ok(sol) => return Ok(sol),
        Err(err) if !is_retryable_startup_error(&err) => return Err(err),
        Err(err) => {
            info!(
                flat_start = primary.flat_start,
                dc_warm_start = primary.dc_warm_start,
                error = %err,
                "adaptive NR startup: primary initialization failed, escalating"
            );
            err
        }
    };

    if !options.flat_start || !options.dc_warm_start {
        let flat_dc = make_startup_retry_options(options, true, true);
        match solve_ac_pf_kernel_core(network, &flat_dc) {
            Ok(sol) => {
                info!("adaptive NR startup: flat/DC retry succeeded");
                return Ok(sol);
            }
            Err(err) if !is_retryable_startup_error(&err) => return Err(err),
            Err(err) => {
                info!(error = %err, "adaptive NR startup: flat/DC retry failed");
                last_retryable_err = err;
            }
        }
    }

    let fdpf_seed_options = make_startup_retry_options(options, true, true);
    if let Some(fdpf_sol) = fdpf_warm_start_attempt(network, &fdpf_seed_options) {
        let mut warm_retry = make_startup_retry_options(options, false, false);
        warm_retry.warm_start = Some(WarmStart::from_solution(&fdpf_sol));
        info!("adaptive NR startup: retrying from FDPF warm start");
        return solve_ac_pf_kernel_core(network, &warm_retry);
    }

    Err(last_retryable_err)
}

/// Attempt FDPF solve to produce a warm-start for a failing NR.
fn fdpf_warm_start_attempt(network: &Network, options: &AcPfOptions) -> Option<PfSolution> {
    use crate::solver::fast_decoupled::FdpfFactors;

    let n = network.n_buses();
    let ybus = build_ybus(network);
    let mut fdpf = FdpfFactors::new(network).ok()?;

    let p_spec = network.bus_p_injection_pu();
    let q_spec = network.bus_q_injection_pu();

    // Initialize voltages (generator setpoints for PV/Slack buses).
    let mut vm: Vec<f64> = network
        .buses
        .iter()
        .map(|b| b.voltage_magnitude_pu)
        .collect();
    let va: Vec<f64> = if options.flat_start {
        // Use DC warm-start angles if available.
        dc_angle_init(network).unwrap_or_else(|| vec![0.0; n])
    } else {
        network.buses.iter().map(|b| b.voltage_angle_rad).collect()
    };

    let bus_map = network.bus_index_map();
    for g in &network.generators {
        if g.can_voltage_regulate()
            && let Some(&gen_idx) = bus_map.get(&g.bus)
            && (network.buses[gen_idx].bus_type == BusType::PV
                || network.buses[gen_idx].bus_type == BusType::Slack)
        {
            // Apply setpoint to remote regulated bus (PSS/E IREG) or own terminal bus.
            let reg = g.reg_bus.unwrap_or(g.bus);
            if let Some(&reg_idx) = bus_map.get(&reg) {
                vm[reg_idx] = g.voltage_setpoint_pu;
            }
        }
    }

    // FDPF with relaxed tolerance — we just need a reasonable starting point.
    let result = fdpf.solve_from_ybus(&ybus, &p_spec, &q_spec, &vm, &va, 1e-4, 500);

    if let Some(fdpf_r) = result {
        let (fdpf_vm, fdpf_va, iters, fdpf_mismatch) =
            (fdpf_r.vm, fdpf_r.va, fdpf_r.iterations, fdpf_r.max_mismatch);
        debug!(
            iterations = iters,
            fdpf_mismatch, "FDPF warm-start converged, retrying NR"
        );
        let (branch_pf, branch_pt, branch_qf, branch_qt) =
            compute_branch_power_flows(network, &fdpf_vm, &fdpf_va, network.base_mva);
        Some(PfSolution {
            pf_model: PfModel::Ac,
            status: SolveStatus::Converged,
            iterations: iters,
            max_mismatch: fdpf_mismatch,
            solve_time_secs: 0.0,
            voltage_magnitude_pu: fdpf_vm,
            voltage_angle_rad: fdpf_va,
            active_power_injection_pu: vec![0.0; n],
            reactive_power_injection_pu: vec![0.0; n],
            branch_p_from_mw: branch_pf,
            branch_p_to_mw: branch_pt,
            branch_q_from_mvar: branch_qf,
            branch_q_to_mvar: branch_qt,
            bus_numbers: network.buses.iter().map(|b| b.number).collect(),
            island_ids: vec![],
            q_limited_buses: vec![],
            n_q_limit_switches: 0,
            gen_slack_contribution_mw: vec![],
            convergence_history: vec![],
            worst_mismatch_bus: None,
            area_interchange: None,
        })
    } else {
        warn!("FDPF warm-start also failed to converge");
        None
    }
}

/// Build a sub-network for a single island (list of global bus indices).
///
/// If the island has no slack bus, the highest-base-kV PV bus (or first bus)
/// is promoted to slack.
pub(crate) fn build_island_network(
    network: &Network,
    island_buses: &[usize],
    bus_map: &HashMap<u32, usize>,
) -> Network {
    use surge_network::network::{Branch, Bus};

    let in_island: std::collections::HashSet<usize> = island_buses.iter().copied().collect();

    let mut local_buses: Vec<Bus> = island_buses
        .iter()
        .map(|&gi| network.buses[gi].clone())
        .collect();

    // Promote a slack bus if none present
    if !local_buses.iter().any(|b| b.bus_type == BusType::Slack) {
        let best = local_buses
            .iter()
            .enumerate()
            .filter(|(_, b)| b.bus_type == BusType::PV)
            .max_by(|(_, a), (_, b)| {
                a.base_kv
                    .partial_cmp(&b.base_kv)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some((best_idx, _)) = best {
            local_buses[best_idx].bus_type = BusType::Slack;
        } else {
            local_buses[0].bus_type = BusType::Slack;
        }
    }

    let global_to_local: HashMap<usize, usize> = island_buses
        .iter()
        .enumerate()
        .map(|(li, &gi)| (gi, li))
        .collect();

    let local_branches: Vec<Branch> = network
        .branches
        .iter()
        .filter(|br| {
            br.in_service
                && bus_map
                    .get(&br.from_bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
                && bus_map
                    .get(&br.to_bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
        })
        .map(|br| {
            let mut b = br.clone();
            let lf = global_to_local[&bus_map[&br.from_bus]];
            let lt = global_to_local[&bus_map[&br.to_bus]];
            b.from_bus = local_buses[lf].number;
            b.to_bus = local_buses[lt].number;
            b
        })
        .collect();

    let local_generators: Vec<surge_network::network::Generator> = network
        .generators
        .iter()
        .filter(|g| {
            g.in_service
                && bus_map
                    .get(&g.bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    let local_loads: Vec<surge_network::network::Load> = network
        .loads
        .iter()
        .filter(|l| {
            bus_map
                .get(&l.bus)
                .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // Point-to-point HVDC links: include if all AC-side terminal buses are in the island.
    let local_hvdc_links: Vec<surge_network::network::HvdcLink> = network
        .hvdc
        .links
        .iter()
        .filter(|link| match link {
            surge_network::network::HvdcLink::Lcc(dc) => {
                bus_map
                    .get(&dc.rectifier.bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
                    && bus_map
                        .get(&dc.inverter.bus)
                        .is_some_and(|&gi| in_island.contains(&gi))
            }
            surge_network::network::HvdcLink::Vsc(vsc) => {
                bus_map
                    .get(&vsc.converter1.bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
                    && bus_map
                        .get(&vsc.converter2.bus)
                        .is_some_and(|&gi| in_island.contains(&gi))
            }
        })
        .cloned()
        .collect();

    // Area interchange records.
    let local_area_schedules: Vec<surge_network::network::AreaSchedule> = network
        .area_schedules
        .iter()
        .filter(|ai| {
            bus_map
                .get(&ai.slack_bus)
                .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // FACTS devices.
    let local_facts_devices: Vec<surge_network::network::FactsDevice> = network
        .facts_devices
        .iter()
        .filter(|f| {
            bus_map
                .get(&f.bus_from)
                .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // Clone-and-filter: start from full parent, then override ALL bus/branch-keyed fields.
    let mut sub_net = network.clone();
    sub_net.buses = local_buses;
    sub_net.branches = local_branches;
    sub_net.generators = local_generators;
    sub_net.loads = local_loads;
    sub_net.hvdc.links = local_hvdc_links;
    sub_net.area_schedules = local_area_schedules;
    sub_net.facts_devices = local_facts_devices;

    // Switched shunts — bus fields are external bus numbers; filter by island membership.
    sub_net.controls.switched_shunts = network
        .controls
        .switched_shunts
        .iter()
        .filter(|ss| {
            bus_map
                .get(&ss.bus)
                .is_some_and(|&bus_idx| in_island.contains(&bus_idx))
        })
        .cloned()
        .collect();

    sub_net.controls.switched_shunts_opf = network
        .controls
        .switched_shunts_opf
        .iter()
        .filter(|ss| {
            bus_map
                .get(&ss.bus)
                .is_some_and(|&bus_idx| in_island.contains(&bus_idx))
        })
        .cloned()
        .collect();

    // Dispatchable loads are bus-number keyed; filter by the current island's
    // bus membership instead of trusting any stale array ordering.
    sub_net.market_data.dispatchable_loads = network
        .market_data
        .dispatchable_loads
        .iter()
        .filter(|dl| {
            bus_map
                .get(&dl.bus)
                .is_some_and(|idx| in_island.contains(idx))
        })
        .cloned()
        .collect();

    // OLTC specs — filter: both from_bus and to_bus must be in island.
    sub_net.controls.oltc_specs = network
        .controls
        .oltc_specs
        .iter()
        .filter(|o| {
            bus_map
                .get(&o.from_bus)
                .is_some_and(|&gi| in_island.contains(&gi))
                && bus_map
                    .get(&o.to_bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // PAR specs — filter: both from_bus and to_bus must be in island.
    sub_net.controls.par_specs = network
        .controls
        .par_specs
        .iter()
        .filter(|p| {
            bus_map
                .get(&p.from_bus)
                .is_some_and(|&gi| in_island.contains(&gi))
                && bus_map
                    .get(&p.to_bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // Explicit DC grids.
    sub_net.hvdc.dc_grids = network
        .hvdc
        .dc_grids
        .iter()
        .filter_map(|grid| {
            let converters: Vec<_> = grid
                .converters
                .iter()
                .filter(|converter| {
                    bus_map
                        .get(&converter.ac_bus())
                        .is_some_and(|&gi| in_island.contains(&gi))
                })
                .cloned()
                .collect();
            if converters.is_empty() {
                return None;
            }
            let retained_dc: HashSet<u32> = converters
                .iter()
                .map(|converter| converter.dc_bus())
                .collect();
            Some(surge_network::network::DcGrid {
                id: grid.id,
                name: grid.name.clone(),
                buses: grid
                    .buses
                    .iter()
                    .filter(|bus| retained_dc.contains(&bus.bus_id))
                    .cloned()
                    .collect(),
                converters,
                branches: grid
                    .branches
                    .iter()
                    .filter(|branch| {
                        retained_dc.contains(&branch.from_bus)
                            && retained_dc.contains(&branch.to_bus)
                    })
                    .cloned()
                    .collect(),
            })
        })
        .collect();

    // Bus-keyed elements.
    sub_net.induction_machines = network
        .induction_machines
        .iter()
        .filter(|im| {
            bus_map
                .get(&im.bus)
                .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();
    sub_net.cim.grounding_impedances = network
        .cim
        .grounding_impedances
        .iter()
        .filter(|gi| {
            bus_map
                .get(&gi.bus)
                .is_some_and(|&idx| in_island.contains(&idx))
        })
        .cloned()
        .collect();
    sub_net.metadata.multi_section_line_groups = network
        .metadata
        .multi_section_line_groups
        .iter()
        .filter(|g| {
            bus_map
                .get(&g.from_bus)
                .is_some_and(|&gi| in_island.contains(&gi))
                && bus_map
                    .get(&g.to_bus)
                    .is_some_and(|&gi| in_island.contains(&gi))
        })
        .cloned()
        .collect();

    // Branch-indexed maps: clear (OPF doesn't use per-island sub-networks).
    sub_net.conditional_limits.clear();

    // Flowgates/interfaces/nomograms have stale branch/flowgate indices in sub-networks.
    sub_net.flowgates.clear();
    sub_net.interfaces.clear();
    sub_net.nomograms.clear();

    sub_net
}

/// Solve AC power flow using Newton-Raphson with KLU sparse LU factorization.
///
/// This is the public entry point for the inner KLU-based solve without
/// interchange or topology reduction outer loops.
pub fn solve_ac_pf_kernel(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    let n = network.n_buses();

    if n == 0 {
        error!("network has no buses");
        return Err(AcPfError::EmptyNetwork);
    }
    if network.slack_bus_index().is_none() {
        error!("network has no slack bus");
        return Err(AcPfError::NoSlackBus);
    }

    validate_branch_endpoints(network)?;

    // Auto-merge zero-impedance branches.
    if options.auto_merge_zero_impedance {
        let merged = crate::topology::zero_impedance::merge_zero_impedance(network, 1e-6);
        if merged.network.buses.len() < network.buses.len() {
            let mut inner_opts = options.clone();
            inner_opts.auto_merge_zero_impedance = false; // prevent recursion
            let inner_sol = solve_ac_pf_kernel(&merged.network, &inner_opts)?;
            return Ok(crate::topology::zero_impedance::expand_pf_solution(
                &inner_sol, &merged, network,
            ));
        }
    }

    if options.warm_start.is_none() && options.startup_policy == StartupPolicy::Adaptive {
        return attempt_adaptive_startup(network, options);
    }

    // Optional warm-start race.
    if !options.flat_start
        && options.warm_start.is_none()
        && options.startup_policy == StartupPolicy::ParallelWarmAndFlat
    {
        let mut flat_opts = options.clone();
        flat_opts.flat_start = true;

        let (warm_result, flat_result) = rayon::join(
            || solve_ac_pf_kernel_core(network, options),
            || solve_ac_pf_kernel_core(network, &flat_opts),
        );

        return match (warm_result, flat_result) {
            (Ok(warm_sol), _) => Ok(warm_sol),
            (Err(_), Ok(flat_sol)) => {
                info!("warm-start NR diverged, flat-start parallel race succeeded");
                Ok(flat_sol)
            }
            (Err(warm_err), Err(_)) => Err(warm_err),
        };
    }

    solve_ac_pf_kernel_core(network, options)
}

pub(crate) fn solve_ac_pf_kernel_core(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    let start = Instant::now();
    let n = network.n_buses();
    let bus_qd_vec = network.bus_load_q_mvar();

    info!(
        buses = n,
        branches = network.branches.len(),
        generators = network.generators.len(),
        flat_start = options.flat_start,
        q_limits = options.enforce_q_limits,
        "starting NR-KLU power flow"
    );

    // Auto-merge switched shunts from network.controls.switched_shunts.
    let merged_opts_storage;
    let options: &AcPfOptions = if options.shunt_enabled
        && options.switched_shunts.is_empty()
        && !network.controls.switched_shunts.is_empty()
    {
        let mut o = options.clone();
        o.switched_shunts = network.controls.switched_shunts.clone();
        merged_opts_storage = o;
        &merged_opts_storage
    } else {
        options
    };

    // Outer discrete-control loop: OLTC transformers and switched shunts.
    let resolved_shunts = if options.shunt_enabled {
        resolve_switched_shunts(network, &options.switched_shunts)?
    } else {
        Vec::new()
    };

    let has_oltc = options.oltc_enabled && !options.oltc_controls.is_empty();
    let has_shunt = options.shunt_enabled && !resolved_shunts.is_empty();
    let has_par = options.par_enabled && !options.par_controls.is_empty();

    if has_oltc || has_shunt || has_par {
        debug!(
            oltc_controls = options.oltc_controls.len(),
            par_controls = options.par_controls.len(),
            switched_shunts = options.switched_shunts.len(),
            "NR-KLU: entering outer discrete-control loop"
        );
        let mut net = network.clone();
        let mut taps: Vec<f64> = net.branches.iter().map(|br| br.tap).collect();
        let mut shifts_deg: Vec<f64> = net
            .branches
            .iter()
            .map(|br| br.phase_shift_rad.to_degrees())
            .collect();
        let mut active_steps: Vec<i32> = resolved_shunts.iter().map(|s| s.n_active_steps).collect();
        // Baseline fixed-shunt susceptance per bus, stored in MVAr on the bus.
        let base_bs_mvar: Vec<f64> = net.buses.iter().map(|b| b.shunt_susceptance_mvar).collect();
        let base_mva = net.base_mva;

        // Apply initial shunt state.
        if has_shunt {
            for (idx, shunt) in resolved_shunts.iter().enumerate() {
                let b_inj_mvar = shunt.b_step * active_steps[idx] as f64 * base_mva;
                let n_buses = net.buses.len();
                let bus = net.buses.get_mut(shunt.bus_idx).ok_or_else(|| {
                    AcPfError::InvalidNetwork(format!(
                        "switched shunt `{}` references bus index {} >= {}",
                        shunt.id, shunt.bus_idx, n_buses,
                    ))
                })?;
                bus.shunt_susceptance_mvar = base_bs_mvar[shunt.bus_idx] + b_inj_mvar;
            }
        }

        let max_outer = options
            .oltc_max_iter
            .max(options.shunt_max_iter)
            .max(options.par_max_iter);
        let mut last_sol: Option<PfSolution> = None;

        for outer_iter in 0..=max_outer {
            if has_oltc {
                for (i, &tap) in taps.iter().enumerate() {
                    net.branches[i].tap = tap;
                }
            }
            if has_par {
                for (i, &shift_deg) in shifts_deg.iter().enumerate() {
                    net.branches[i].phase_shift_rad = shift_deg.to_radians();
                }
            }
            let mut inner_opts = options.clone();
            if let Some(ref prev_sol) = last_sol {
                inner_opts.warm_start = Some(WarmStart::from_solution(prev_sol));
            }
            // Disable re-entrant discrete control in the recursive call.
            inner_opts.oltc_controls = Vec::new();
            inner_opts.par_controls = Vec::new();
            inner_opts.par_enabled = false;
            inner_opts.switched_shunts = Vec::new();
            inner_opts.shunt_enabled = false;

            let sol = solve_ac_pf_kernel_core(&net, &inner_opts)?;

            let mut n_changed = 0usize;
            if has_oltc && outer_iter < options.oltc_max_iter {
                n_changed +=
                    apply_oltc_steps(&options.oltc_controls, &sol.voltage_magnitude_pu, &mut taps);
            }
            if has_par && outer_iter < options.par_max_iter {
                n_changed += apply_par_steps(&options.par_controls, &sol, &mut shifts_deg);
            }
            if has_shunt && outer_iter < options.shunt_max_iter {
                n_changed += apply_shunt_steps(
                    &resolved_shunts,
                    &sol.voltage_magnitude_pu,
                    &mut active_steps,
                );
                for (idx, shunt) in resolved_shunts.iter().enumerate() {
                    let b_inj_mvar = shunt.b_step * active_steps[idx] as f64 * base_mva;
                    let bus = {
                        let n_buses = net.buses.len();
                        net.buses.get_mut(shunt.bus_idx).ok_or_else(|| {
                            AcPfError::InvalidNetwork(format!(
                                "switched shunt `{}` references bus index {} >= {}",
                                shunt.id, shunt.bus_idx, n_buses,
                            ))
                        })?
                    };
                    bus.shunt_susceptance_mvar = base_bs_mvar[shunt.bus_idx] + b_inj_mvar;
                }
            }
            last_sol = Some(sol);
            if n_changed == 0 {
                break;
            }
        }

        return last_sol.map(Ok).unwrap_or(Err(AcPfError::NotConverged {
            iterations: options.max_iterations,
            max_mismatch: f64::INFINITY,
            worst_bus: None,
            partial_vm: None,
            partial_va: None,
        }));
    }

    // Island decomposition.
    if options.detect_islands {
        let bus_map = network.bus_index_map();
        let islands = detect_islands(network, &bus_map);
        if islands.n_islands > 1 {
            info!(islands = islands.n_islands, "multi-island KLU solve");
            return solve_ac_pf_kernel_multi_island(network, options, &islands, start);
        }
    }

    debug!(buses = n, "building Y-bus for NR-KLU solve");
    let mut ybus = build_ybus(network);

    // Mutable working copy of bus types.
    let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();

    let bus_map = network.bus_index_map();

    // Enforce generator P-limits.
    if options.enforce_gen_p_limits {
        let pv_before = bus_types.iter().filter(|&&bt| bt == BusType::PV).count();
        apply_generator_p_limit_demotions(network, &mut bus_types);
        let pv_after = bus_types.iter().filter(|&&bt| bt == BusType::PV).count();
        let pv_demoted = pv_before.saturating_sub(pv_after) as u32;
        if pv_demoted > 0 {
            info!(
                pv_demoted,
                "enforce_gen_p_limits: demoted PV buses with infeasible P setpoints"
            );
        }
    }

    // FACTS control.
    let mut facts_states = crate::control::facts::FactsStates::new(network, &bus_map);
    if !facts_states.is_empty() {
        facts_states.apply_svc_to_ybus(&mut ybus);
        facts_states.apply_tcsc_to_ybus(&mut ybus);
        debug!(
            svcs = facts_states.svcs.len(),
            tcscs = facts_states.tcscs.len(),
            "FACTS outer-loop control active"
        );
    }

    // Remote voltage regulation map.
    let remote_reg_map = build_remote_reg_map(network, &bus_map, &bus_types);

    // Per-generator Q-limit tracking.
    let (mut gen_q_states, bus_gen_map, _buses_with_inf_q) = if options.enforce_q_limits {
        build_gen_q_states(network, &remote_reg_map.terminal_demote)
    } else {
        (Vec::new(), HashMap::new(), std::collections::HashSet::new())
    };
    let has_q_limits = !bus_gen_map.is_empty();

    let mut p_spec_base = network.bus_p_injection_pu();
    let mut q_spec_base = network.bus_q_injection_pu();

    let zip_bus_data = build_zip_bus_data(network);

    let mut participation: Option<HashMap<usize, f64>> = build_participation_map(network, options);

    // Initialize voltages.
    let mut vm: Vec<f64> = if options.flat_start {
        vec![1.0; n]
    } else {
        network
            .buses
            .iter()
            .map(|b| b.voltage_magnitude_pu)
            .collect()
    };
    let mut va: Vec<f64> = if options.flat_start {
        vec![0.0; n]
    } else {
        network.buses.iter().map(|b| b.voltage_angle_rad).collect()
    };

    // Preserve slack bus Va from case file even on flat start.
    if options.flat_start {
        for (i, bus) in network.buses.iter().enumerate() {
            if bus.bus_type == BusType::Slack {
                va[i] = bus.voltage_angle_rad;
            }
        }
    }

    if let Some(ref prior) = options.warm_start
        && prior.vm.len() == n
        && prior.va.len() == n
    {
        vm.copy_from_slice(&prior.vm);
        va.copy_from_slice(&prior.va);
    }

    // DC warm-start.
    if options.flat_start
        && options.dc_warm_start
        && options.warm_start.is_none()
        && let Some(dc_angles) = dc_angle_init(network)
    {
        va.copy_from_slice(&dc_angles);
    }

    apply_generator_voltage_setpoints(network, &bus_map, &bus_types, &mut vm);

    let bus_numbers: Vec<u32> = network.buses.iter().map(|b| b.number).collect();

    // Q-limit switching state.
    let mut switched_to_pq = vec![false; n];

    // Per-bus PV/PQ flip count for oscillation detection.
    let mut bus_flip_count = vec![0u32; n];

    reclassify_dead_pv_buses(network, &mut bus_types, &mut switched_to_pq);

    // Remote voltage regulation (PSS/E IREG).
    apply_remote_reg_types(
        &remote_reg_map,
        &mut bus_types,
        &mut vm,
        &mut switched_to_pq,
        None,
    );

    // Record original slack bus and its initial angle.
    let orig_ref_idx = bus_types
        .iter()
        .position(|t| *t == BusType::Slack)
        .unwrap_or(0);
    let va_ref0 = va[orig_ref_idx];

    let mut n_q_limit_switches_total = 0u32;
    let mut convergence_history: Vec<(u32, f64)> = Vec::new();
    let mut last_worst_internal_idx: usize = 0;

    // Stott-Alsac augmented Jacobian: accumulated imbalance variable λ.
    let mut lambda: f64 = 0.0;

    let has_facts = !facts_states.is_empty();
    let max_q_outer: u32 = if !has_q_limits && !has_facts {
        1
    } else if !has_q_limits {
        20
    } else {
        options.max_iterations.min(100)
    };

    let (mut pvpq_indices, mut pq_indices) = classify_indices(&bus_types);

    let mut workspace = NrWorkspace::new(n, !zip_bus_data.is_empty());
    let mut last_iteration = 0u32;
    let mut last_mismatch = f64::INFINITY;

    // Outer-loop divergence detection.
    let mut best_outer_mismatch = f64::INFINITY;
    let mut outer_no_progress: u32 = 0;
    const OUTER_STALL_LIMIT: u32 = 5;

    for q_outer_iter in 0..=max_q_outer {
        // Build fused pattern for current bus type configuration.
        let fused_pattern =
            crate::matrix::fused::FusedPattern::new(&ybus, &pvpq_indices, &pq_indices);
        let dim = fused_pattern.dim();

        let symbolic_ref = fused_pattern.symbolic().as_ref();
        let col_ptrs: Vec<usize> = symbolic_ref.col_ptr().to_vec();
        let row_indices_klu: Vec<usize> = symbolic_ref.row_idx().to_vec();

        let mut klu = surge_sparse::KluSolver::new(dim, &col_ptrs, &row_indices_klu)
            .map_err(|e| AcPfError::InvalidNetwork(format!("KLU symbolic analysis failed: {e}")))?;

        workspace.prepare_factor_buffers(fused_pattern.nnz(), dim);

        lambda = 0.0;
        let aug = participation.as_ref().map(|pmap| {
            build_augmented_slack_data(&bus_types, pmap, &pvpq_indices, &pq_indices, dim)
        });

        let inner = run_nr_inner(
            PreparedNrModel {
                ybus: &ybus,
                fused_pattern: &fused_pattern,
                p_spec_base: &p_spec_base,
                q_spec_base: &q_spec_base,
                zip_bus_data: &zip_bus_data,
                participation: participation.as_ref(),
                pvpq_indices: &pvpq_indices,
                pq_indices: &pq_indices,
                aug: aug.as_ref(),
                options: NrKernelOptions {
                    tolerance: options.tolerance,
                    max_iterations: nr_inner_iteration_limit(options),
                    stall_limit: nr_inner_stall_limit(options),
                    vm_min: options.vm_min,
                    vm_max: options.vm_max,
                    line_search: options.line_search,
                    allow_partial_nonconverged: has_q_limits,
                },
            },
            NrState {
                vm: &mut vm,
                va: &mut va,
                lambda: &mut lambda,
            },
            &mut workspace,
            &mut klu,
            if options.record_convergence_history {
                Some(&mut convergence_history)
            } else {
                None
            },
        )
        .map_err(|failure| AcPfError::NotConverged {
            iterations: failure.iterations,
            max_mismatch: failure.max_mismatch,
            worst_bus: failure
                .worst_internal_idx
                .and_then(|idx| bus_numbers.get(idx).copied()),
            partial_vm: Some(vm.clone()),
            partial_va: Some(va.clone()),
        })?;

        if inner.converged {
            debug!(
                iteration = inner.iterations,
                max_mismatch = inner.max_mismatch,
                "NR-KLU inner loop converged"
            );
        } else {
            warn!(
                iteration = inner.iterations,
                max_mismatch = inner.max_mismatch,
                "NR-KLU did not converge within inner-loop budget"
            );
        }

        last_iteration = inner.iterations;
        last_mismatch = inner.max_mismatch;
        last_worst_internal_idx = inner.worst_internal_idx;
        let max_mismatch = inner.max_mismatch;

        // FACTS outer-loop control.
        if has_facts && max_mismatch < options.tolerance {
            let svc_lim = facts_states.check_svc_limits(&mut ybus);
            let tcsc_lim = facts_states.check_tcsc_limits(&mut ybus);
            if svc_lim + tcsc_lim > 0 {
                debug!(
                    svc_limits = svc_lim,
                    tcsc_limits = tcsc_lim,
                    "FACTS devices hit limits — re-solving"
                );
                continue;
            }

            let n_svc = facts_states.update_svc_susceptances(&vm, &mut ybus);
            let n_tcsc = facts_states.update_tcsc_reactances(&vm, &va, &mut ybus);
            if n_svc + n_tcsc > 0 {
                debug!(
                    svc_updates = n_svc,
                    tcsc_updates = n_tcsc,
                    "FACTS outer-loop: Y-bus updated, re-solving"
                );
                continue;
            }
        }

        // Q-limit check (outer iteration).
        if !has_q_limits || q_outer_iter >= max_q_outer {
            if max_mismatch >= options.tolerance {
                return Err(AcPfError::NotConverged {
                    iterations: last_iteration,
                    max_mismatch,
                    worst_bus: bus_numbers.get(last_worst_internal_idx).copied(),
                    partial_vm: Some(vm.clone()),
                    partial_va: Some(va.clone()),
                });
            }
            break;
        }

        if max_mismatch >= options.tolerance {
            return Err(AcPfError::NotConverged {
                iterations: last_iteration,
                max_mismatch,
                worst_bus: bus_numbers.get(last_worst_internal_idx).copied(),
                partial_vm: Some(vm.clone()),
                partial_va: Some(va.clone()),
            });
        }

        // Outer-loop stall detection.
        if has_q_limits && max_mismatch >= options.tolerance {
            if max_mismatch < best_outer_mismatch * 0.99 {
                best_outer_mismatch = max_mismatch;
                outer_no_progress = 0;
            } else {
                outer_no_progress += 1;
                if outer_no_progress >= OUTER_STALL_LIMIT {
                    debug!(
                        q_outer_iter,
                        max_mismatch, "NR-KLU outer loop stalled — bailing for fallback"
                    );
                    return Err(AcPfError::NotConverged {
                        iterations: last_iteration,
                        max_mismatch,
                        worst_bus: bus_numbers.get(last_worst_internal_idx).copied(),
                        partial_vm: Some(vm.clone()),
                        partial_va: Some(va.clone()),
                    });
                }
            }
        }

        // Update P-spec for current slack bus(es).
        for i in 0..n {
            if bus_types[i] == BusType::Slack && q_outer_iter == 0 {
                p_spec_base[i] = workspace.p_calc()[i];
            }
        }

        // Snapshot bus types before Q-limit enforcement for oscillation detection.
        let bus_types_before: Vec<BusType> = bus_types.to_vec();

        let round_switches = apply_per_gen_q_limits(
            &mut bus_types,
            workspace.q_calc(),
            &mut gen_q_states,
            &bus_gen_map,
            &mut q_spec_base,
            &mut switched_to_pq,
            network,
            &bus_qd_vec,
            &remote_reg_map.terminal_demote,
            options.skip_slack_q_limits,
            options.incremental_q_limits,
            options.q_sharing,
        )?;

        n_q_limit_switches_total += round_switches;

        // Track PV/PQ oscillations and lock buses that flip > 3 times.
        for i in 0..n {
            if bus_types[i] != bus_types_before[i] {
                bus_flip_count[i] += 1;
                if bus_flip_count[i] > 3 && bus_types[i] != BusType::Slack && !switched_to_pq[i] {
                    warn!(
                        bus_idx = i,
                        flips = bus_flip_count[i],
                        "NR-KLU Q-limit oscillation detected, locking bus in PQ state"
                    );
                    bus_types[i] = BusType::PQ;
                    switched_to_pq[i] = true;
                }
            }
        }

        if round_switches == 0 {
            if max_mismatch >= options.tolerance {
                return Err(AcPfError::NotConverged {
                    iterations: last_iteration,
                    max_mismatch,
                    worst_bus: bus_numbers.get(last_worst_internal_idx).copied(),
                    partial_vm: Some(vm.clone()),
                    partial_va: Some(va.clone()),
                });
            }
            break; // No bus type changes — solution accepted.
        }

        // Rebuild participation map after bus type changes.
        if let Some(ref mut pmap) = participation {
            pmap.retain(|&bus_idx, _| {
                bus_types[bus_idx] == BusType::PV || bus_types[bus_idx] == BusType::Slack
            });
            let sum: f64 = pmap.values().sum();
            if sum > 1e-12 {
                for alpha in pmap.values_mut() {
                    *alpha /= sum;
                }
            } else {
                participation = None;
            }
        }

        // Remote voltage regulation: re-apply type switching after Q-limit changes.
        if !remote_reg_map.remote_controllers.is_empty() {
            let mut blocked_remote_promotions = HashSet::new();
            for (&remote_idx, controllers) in &remote_reg_map.remote_controllers {
                let any_active = controllers
                    .iter()
                    .any(|&term_idx| !switched_to_pq[term_idx]);
                if !any_active && bus_types[remote_idx] == BusType::PV {
                    bus_types[remote_idx] = BusType::PQ;
                    blocked_remote_promotions.insert(remote_idx);
                }
            }
            apply_remote_reg_types(
                &remote_reg_map,
                &mut bus_types,
                &mut vm,
                &mut switched_to_pq,
                Some(&blocked_remote_promotions),
            );
        }

        // Rebuild index sets for next inner NR pass.
        let (new_pvpq, new_pq) = classify_indices(&bus_types);
        pvpq_indices = new_pvpq;
        pq_indices = new_pq;
    }

    // Final convergence check.
    if last_mismatch >= options.tolerance {
        return Err(AcPfError::NotConverged {
            iterations: last_iteration,
            max_mismatch: last_mismatch,
            worst_bus: bus_numbers.get(last_worst_internal_idx).copied(),
            partial_vm: Some(vm.clone()),
            partial_va: Some(va.clone()),
        });
    }

    let solve_time = start.elapsed().as_secs_f64();
    info!(
        iterations = last_iteration,
        q_switches = n_q_limit_switches_total,
        max_mismatch = last_mismatch,
        solve_time_ms = format_args!("{:.3}", solve_time * 1000.0),
        "NR-KLU converged"
    );

    apply_angle_reference(
        &mut va,
        orig_ref_idx,
        va_ref0,
        options.angle_reference,
        network,
    );

    let meta = build_nr_meta(
        network,
        &bus_numbers,
        &switched_to_pq,
        n_q_limit_switches_total,
        &participation,
        lambda,
        options,
    );

    Ok(build_solution(
        network,
        &vm,
        &va,
        workspace.p_calc(),
        workspace.q_calc(),
        last_iteration,
        last_mismatch,
        solve_time,
        bus_numbers,
        meta,
        convergence_history,
    ))
}

/// Multi-island KLU solve.
fn solve_ac_pf_kernel_multi_island(
    network: &Network,
    options: &AcPfOptions,
    islands: &crate::topology::islands::IslandInfo,
    start: Instant,
) -> Result<PfSolution, AcPfError> {
    use rayon::prelude::*;

    let n = network.n_buses();
    let bus_map = network.bus_index_map();

    let island_results: Vec<(usize, &Vec<usize>, Result<PfSolution, AcPfError>)> = islands
        .components
        .par_iter()
        .enumerate()
        .map(|(island_id, island_buses)| {
            // Single-bus island: trivial.
            if island_buses.len() == 1 {
                return (
                    island_id,
                    island_buses,
                    Ok(build_single_bus_island_solution(
                        network,
                        island_buses,
                        island_id,
                        &bus_map,
                    )),
                );
            }

            let sub_net = build_island_network(network, island_buses, &bus_map);

            // Remap global slack_participation indices to local indices.
            let remapped_participation: Option<HashMap<usize, f64>> =
                if let Some(ref global_pmap) = options.slack_participation {
                    let global_to_local: HashMap<usize, usize> = island_buses
                        .iter()
                        .enumerate()
                        .map(|(li, &gi)| (gi, li))
                        .collect();
                    let local_map: HashMap<usize, f64> = global_pmap
                        .iter()
                        .filter_map(|(&gi, &alpha)| global_to_local.get(&gi).map(|&li| (li, alpha)))
                        .collect();
                    if local_map.is_empty() {
                        None
                    } else {
                        let sum: f64 = local_map.values().sum();
                        if sum > 1e-12 {
                            Some(local_map.into_iter().map(|(k, v)| (k, v / sum)).collect())
                        } else {
                            None
                        }
                    }
                } else {
                    None
                };

            let remapped_generator_participation: Option<HashMap<usize, f64>> =
                if let Some(ref global_gmap) = options.generator_slack_participation {
                    let island_bus_set: HashSet<usize> = island_buses.iter().copied().collect();
                    let mut local_gen_idx = 0usize;
                    let global_to_local: HashMap<usize, usize> = network
                        .generators
                        .iter()
                        .enumerate()
                        .filter_map(|(gi, generator)| {
                            if !generator.in_service {
                                return None;
                            }
                            let &bus_idx = bus_map.get(&generator.bus)?;
                            if !island_bus_set.contains(&bus_idx) {
                                return None;
                            }
                            let mapped = (gi, local_gen_idx);
                            local_gen_idx += 1;
                            Some(mapped)
                        })
                        .collect();
                    let local_map: HashMap<usize, f64> = global_gmap
                        .iter()
                        .filter_map(|(&gi, &alpha)| global_to_local.get(&gi).map(|&li| (li, alpha)))
                        .collect();
                    if local_map.is_empty() {
                        None
                    } else {
                        let sum: f64 = local_map.values().sum();
                        if sum > 1e-12 {
                            Some(local_map.into_iter().map(|(k, v)| (k, v / sum)).collect())
                        } else {
                            None
                        }
                    }
                } else {
                    None
                };

            let sub_distributed_slack = options.distributed_slack
                && options.slack_participation.is_none()
                && options.generator_slack_participation.is_none();
            let sub_opts = AcPfOptions {
                detect_islands: false,
                slack_participation: remapped_participation,
                generator_slack_participation: remapped_generator_participation,
                distributed_slack: sub_distributed_slack,
                ..options.clone()
            };
            let result = solve_ac_pf_kernel_core(&sub_net, &sub_opts);

            // Per-island FDPF fallback.
            let result = match result {
                Ok(sol) => Ok(sol),
                Err(_) => {
                    debug!(
                        island = island_id,
                        buses = island_buses.len(),
                        "island KLU failed, trying FDPF warm-start"
                    );
                    if let Some(fdpf_sol) = fdpf_warm_start_attempt(&sub_net, &sub_opts) {
                        let mut retry_opts = sub_opts.clone();
                        retry_opts.warm_start = Some(WarmStart::from_solution(&fdpf_sol));
                        retry_opts.flat_start = false;
                        solve_ac_pf_kernel_core(&sub_net, &retry_opts)
                    } else {
                        Err(AcPfError::NotConverged {
                            iterations: 0,
                            max_mismatch: f64::INFINITY,
                            worst_bus: None,
                            partial_vm: None,
                            partial_va: None,
                        })
                    }
                }
            };
            (island_id, island_buses, result)
        })
        .collect();

    let mut vm_all = vec![1.0f64; n];
    let mut va_all = vec![0.0f64; n];
    let mut p_all = vec![0.0f64; n];
    let mut q_all = vec![0.0f64; n];
    let mut island_ids = vec![0usize; n];
    let mut q_limited_buses = Vec::new();
    let mut n_q_limit_switches = 0u32;
    let mut gen_slack_contribution_mw: Option<Vec<f64>> = None;
    let mut total_iterations = 0u32;
    let mut max_mismatch_all = 0.0f64;

    for (island_id, island_buses, result) in island_results {
        let sub_sol = result?;
        q_limited_buses.extend(sub_sol.q_limited_buses.iter().copied());
        n_q_limit_switches += sub_sol.n_q_limit_switches;

        if !sub_sol.gen_slack_contribution_mw.is_empty() {
            let global_contrib = gen_slack_contribution_mw
                .get_or_insert_with(|| vec![0.0; network.generators.len()]);
            let island_bus_set: HashSet<usize> = island_buses.iter().copied().collect();
            let mut island_contrib_iter = sub_sol.gen_slack_contribution_mw.iter().copied();
            for (gen_idx, generator) in network.generators.iter().enumerate() {
                let Some(&bus_idx) = bus_map.get(&generator.bus) else {
                    continue;
                };
                if !island_bus_set.contains(&bus_idx) {
                    continue;
                }
                if let Some(contrib) = island_contrib_iter.next() {
                    global_contrib[gen_idx] = contrib;
                }
            }
            debug_assert!(
                island_contrib_iter.next().is_none(),
                "island generator contribution count must match the original generator ordering"
            );
        }
        for (local_idx, &global_idx) in island_buses.iter().enumerate() {
            vm_all[global_idx] = sub_sol.voltage_magnitude_pu[local_idx];
            va_all[global_idx] = sub_sol.voltage_angle_rad[local_idx];
            p_all[global_idx] = sub_sol.active_power_injection_pu[local_idx];
            q_all[global_idx] = sub_sol.reactive_power_injection_pu[local_idx];
            island_ids[global_idx] = island_id;
        }
        total_iterations = total_iterations.max(sub_sol.iterations);
        max_mismatch_all = max_mismatch_all.max(sub_sol.max_mismatch);
    }

    let solve_time = start.elapsed().as_secs_f64();
    info!(
        islands = islands.n_islands,
        max_mismatch = max_mismatch_all,
        solve_time_ms = format_args!("{:.3}", solve_time * 1000.0),
        "NR-KLU multi-island converged"
    );

    Ok(build_solution(
        network,
        &vm_all,
        &va_all,
        &p_all,
        &q_all,
        total_iterations,
        max_mismatch_all,
        solve_time,
        network.buses.iter().map(|b| b.number).collect(),
        NrMeta {
            island_ids,
            q_limited_buses,
            n_q_limit_switches,
            gen_slack_contribution_mw: gen_slack_contribution_mw.unwrap_or_default(),
        },
        Vec::new(), // multi-island: no single convergence_history
    ))
}

fn build_single_bus_island_solution(
    network: &Network,
    island_buses: &[usize],
    island_id: usize,
    bus_map: &HashMap<u32, usize>,
) -> PfSolution {
    let sub_net = build_island_network(network, island_buses, bus_map);
    debug_assert_eq!(sub_net.n_buses(), 1);
    let vm = vec![sub_net.buses[0].voltage_magnitude_pu];
    let va = vec![sub_net.buses[0].voltage_angle_rad];
    let ybus = build_ybus(&sub_net);
    let (p_calc, q_calc) = compute_power_injection(&ybus, &vm, &va);
    build_solution(
        &sub_net,
        &vm,
        &va,
        &p_calc,
        &q_calc,
        0,
        0.0,
        0.0,
        sub_net.buses.iter().map(|bus| bus.number).collect(),
        NrMeta {
            island_ids: vec![island_id],
            ..NrMeta::default()
        },
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_solution(
    network: &Network,
    vm: &[f64],
    va: &[f64],
    p_calc: &[f64],
    q_calc: &[f64],
    iterations: u32,
    max_mismatch: f64,
    solve_time_secs: f64,
    bus_numbers: Vec<u32>,
    meta: NrMeta,
    convergence_history: Vec<(u32, f64)>,
) -> PfSolution {
    let (branch_pf, branch_pt, branch_qf, branch_qt) =
        compute_branch_power_flows(network, vm, va, network.base_mva);
    PfSolution {
        pf_model: PfModel::Ac,
        status: SolveStatus::Converged,
        iterations,
        max_mismatch,
        solve_time_secs,
        voltage_magnitude_pu: vm.to_vec(),
        voltage_angle_rad: va.to_vec(),
        active_power_injection_pu: p_calc.to_vec(),
        reactive_power_injection_pu: q_calc.to_vec(),
        branch_p_from_mw: branch_pf,
        branch_p_to_mw: branch_pt,
        branch_q_from_mvar: branch_qf,
        branch_q_to_mvar: branch_qt,
        bus_numbers,
        island_ids: meta.island_ids,
        q_limited_buses: meta.q_limited_buses,
        n_q_limit_switches: meta.n_q_limit_switches,
        gen_slack_contribution_mw: meta.gen_slack_contribution_mw,
        convergence_history,
        worst_mismatch_bus: None, // None on successful convergence
        area_interchange: None,
    }
}
