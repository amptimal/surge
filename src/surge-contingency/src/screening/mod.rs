// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LODF and FDPF contingency screening passes.

pub mod lodf;

use std::collections::HashMap;
use std::sync::Mutex;

use rayon::prelude::*;
use surge_ac::FdpfFactors;
use surge_ac::matrix::ybus::build_ybus_from_parts;
use surge_network::Network;
use surge_network::network::Contingency;
use surge_solution::PfSolution;
use tracing::info;

use crate::engine::parallel::solve_contingencies_parallel;
use crate::types::{ContingencyError, ContingencyOptions, ContingencyResult, ContingencyStatus};
use crate::violations::detect_violations_from_parts;

// ---------------------------------------------------------------------------
// LODF screening
// ---------------------------------------------------------------------------

/// Screen contingencies using LODF to identify critical ones.
///
/// Returns (critical_indices, screened_out_count).
pub(crate) fn screen_with_lodf(
    network: &Network,
    contingencies: &[Contingency],
    options: &ContingencyOptions,
    base_case: &PfSolution,
) -> Result<(Vec<usize>, usize), ContingencyError> {
    let thermal_tier = options.thermal_rating;
    let rated_count = network
        .branches
        .iter()
        .filter(|b| b.in_service && crate::types::get_rating(b, thermal_tier) > 0.0)
        .count();
    if rated_count == 0 {
        info!("No rated branches found; sending all contingencies to AC solve");
        return Ok(((0..contingencies.len()).collect(), 0));
    }

    let (mut critical, _screened_count) = {
        let branch_flows = surge_dc::solve_dc(network)
            .map_err(|e| ContingencyError::DcSolveFailed(e.to_string()))?
            .branch_p_flow;
        self::lodf::screen_with_sparse_lodf(
            network,
            contingencies,
            &branch_flows,
            options.lodf_screening_threshold * 100.0,
            options.thermal_rating,
        )
    };

    // Optionally run a voltage screening pass (FDPF) for contingencies that
    // thermal LODF did not already mark as critical.
    if !options.voltage_pre_screen {
        let final_screened = contingencies.len() - critical.len();
        return Ok((critical, final_screened));
    }

    let critical_set: std::collections::HashSet<usize> = critical.iter().cloned().collect();
    let bus_map = network.bus_index_map();
    let p_spec = network.bus_p_injection_pu();
    let q_spec = network.bus_q_injection_pu();
    let base_ybus = build_ybus_from_parts(
        &network.branches,
        &network.buses,
        network.base_mva,
        &bus_map,
        &network.metadata.impedance_corrections,
    );

    let corr_map: HashMap<
        u32,
        &surge_network::network::impedance_correction::ImpedanceCorrectionTable,
    > = network
        .metadata
        .impedance_corrections
        .iter()
        .map(|t| (t.number, t))
        .collect();

    // Pre-compute Y-bus deltas for branch contingencies
    let branch_deltas =
        crate::engine::compute_branch_deltas(contingencies.iter(), network, &bus_map, &corr_map);

    // Parallel FDPF voltage screening
    let fdpf_pool: Mutex<Vec<FdpfFactors>> = Mutex::new(Vec::new());
    let fdpf_tolerance = 1e-4_f64;
    let fdpf_max_iters = options.fdpf_max_iterations;

    let candidates: Vec<usize> = (0..contingencies.len())
        .filter(|&idx| {
            !critical_set.contains(&idx)
                && contingencies[idx].generator_indices.is_empty()
                && branch_deltas[idx].is_some()
        })
        .collect();

    let voltage_critical: Vec<usize> = candidates
        .par_iter()
        .filter_map(|&ctg_idx| {
            let deltas = branch_deltas[ctg_idx].as_ref()?;

            let mut fdpf = match fdpf_pool
                .lock()
                .expect("fdpf_pool mutex should not be poisoned")
                .pop()
            {
                Some(f) => f,
                None => match FdpfFactors::new(network) {
                    Ok(f) => f,
                    Err(_) => return Some(ctg_idx),
                },
            };

            let mut ybus = base_ybus.clone();
            for delta in deltas {
                ybus.apply_deltas(delta);
            }

            let fdpf_result = fdpf.solve_from_ybus(
                &ybus,
                &p_spec,
                &q_spec,
                &base_case.voltage_magnitude_pu,
                &base_case.voltage_angle_rad,
                fdpf_tolerance,
                fdpf_max_iters,
            );

            fdpf_pool
                .lock()
                .expect("fdpf_pool mutex should not be poisoned")
                .push(fdpf);

            match fdpf_result {
                Some(r) => {
                    let has_voltage_violation =
                        r.vm.iter().zip(network.buses.iter()).any(|(&v, bus)| {
                            let vmin = if bus.voltage_min_pu > 0.0 {
                                bus.voltage_min_pu
                            } else {
                                options.vm_min
                            };
                            let vmax = if bus.voltage_max_pu > 0.0 {
                                bus.voltage_max_pu
                            } else {
                                options.vm_max
                            };
                            v < vmin || v > vmax
                        });
                    if has_voltage_violation {
                        Some(ctg_idx)
                    } else {
                        None
                    }
                }
                None => Some(ctg_idx),
            }
        })
        .collect();

    let voltage_critical_added = voltage_critical.len();
    critical.extend(voltage_critical);

    if voltage_critical_added > 0 {
        info!(
            "Voltage screening: {} additional contingencies flagged for AC solve (voltage violations)",
            voltage_critical_added
        );
        critical.sort_unstable();
        critical.dedup();
    }

    let final_screened = contingencies.len() - critical.len();
    Ok((critical, final_screened))
}

// ---------------------------------------------------------------------------
// FDPF two-pass screening
// ---------------------------------------------------------------------------

/// Two-pass contingency analysis: FDPF screening → NR confirmation.
pub(crate) fn screen_and_solve_with_fdpf(
    network: &Network,
    contingencies: &[Contingency],
    options: &ContingencyOptions,
    base_case: &PfSolution,
) -> Result<Vec<ContingencyResult>, ContingencyError> {
    let bus_map = network.bus_index_map();
    let p_spec = network.bus_p_injection_pu();
    let q_spec = network.bus_q_injection_pu();

    // Build base Y-bus once
    let base_ybus = build_ybus_from_parts(
        &network.branches,
        &network.buses,
        network.base_mva,
        &bus_map,
        &network.metadata.impedance_corrections,
    );

    let corr_map: HashMap<
        u32,
        &surge_network::network::impedance_correction::ImpedanceCorrectionTable,
    > = network
        .metadata
        .impedance_corrections
        .iter()
        .map(|t| (t.number, t))
        .collect();

    // Pre-compute branch removal deltas
    let branch_deltas =
        crate::engine::compute_branch_deltas(contingencies.iter(), network, &bus_map, &corr_map);

    // Pass 1: Parallel FDPF screening
    let fdpf_pool: Mutex<Vec<FdpfFactors>> = Mutex::new(Vec::new());
    let fdpf_tolerance = 1e-4;
    let fdpf_max_iters = options.fdpf_max_iterations;

    let screening_results: Vec<(usize, Option<ContingencyResult>)> = contingencies
        .par_iter()
        .zip(branch_deltas.par_iter())
        .enumerate()
        .map(|(idx, (ctg, deltas_opt))| {
            if let Some(deltas) = deltas_opt {
                let mut fdpf = match fdpf_pool
                    .lock()
                    .expect("fdpf_pool mutex should not be poisoned")
                    .pop()
                {
                    Some(f) => f,
                    None => match FdpfFactors::new(network) {
                        Ok(f) => f,
                        Err(_) => return (idx, None),
                    },
                };

                let mut ybus = base_ybus.clone();
                for delta in deltas {
                    ybus.apply_deltas(delta);
                }

                let fdpf_result = fdpf.solve_from_ybus(
                    &ybus,
                    &p_spec,
                    &q_spec,
                    &base_case.voltage_magnitude_pu,
                    &base_case.voltage_angle_rad,
                    fdpf_tolerance,
                    fdpf_max_iters,
                );

                fdpf_pool
                    .lock()
                    .expect("fdpf_pool mutex should not be poisoned")
                    .push(fdpf);

                match fdpf_result {
                    Some(r) => {
                        let outaged_fdpf: std::collections::HashSet<usize> =
                            ctg.branch_indices.iter().copied().collect();
                        let violations = detect_violations_from_parts(
                            &network.branches,
                            &network.buses,
                            network.base_mva,
                            &bus_map,
                            Some(&outaged_fdpf),
                            &r.vm,
                            &r.va,
                            options,
                            &network.flowgates,
                            &network.interfaces,
                        );
                        if violations.is_empty() {
                            let (post_vm, post_va, post_branch_flows) =
                                if options.store_post_voltages {
                                    (
                                        Some(r.vm.clone()),
                                        Some(r.va.clone()),
                                        Some(crate::violations::compute_branch_flows_mva(
                                            &network.branches,
                                            network.base_mva,
                                            &bus_map,
                                            Some(&outaged_fdpf),
                                            &r.vm,
                                            &r.va,
                                        )),
                                    )
                                } else {
                                    (None, None, None)
                                };
                            (
                                idx,
                                Some(ContingencyResult {
                                    id: ctg.id.clone(),
                                    label: ctg.label.clone(),
                                    branch_indices: ctg.branch_indices.clone(),
                                    generator_indices: ctg.generator_indices.clone(),
                                    status: ContingencyStatus::Approximate,
                                    converged: true,
                                    iterations: r.iterations,
                                    n_islands: 1,
                                    post_vm,
                                    post_va,
                                    post_branch_flows,
                                    tpl_category: ctg.tpl_category,
                                    ..Default::default()
                                }),
                            )
                        } else {
                            (idx, None)
                        }
                    }
                    None => (idx, None),
                }
            } else {
                (idx, None)
            }
        })
        .collect();

    let mut ordered_results: Vec<Option<ContingencyResult>> = vec![None; contingencies.len()];
    let mut critical_indices: Vec<usize> = Vec::new();

    for (idx, result) in screening_results {
        match result {
            Some(r) => ordered_results[idx] = Some(r),
            None => critical_indices.push(idx),
        }
    }

    let screened_count = ordered_results
        .iter()
        .filter(|result| result.is_some())
        .count();
    let critical_count = critical_indices.len();

    info!(
        "FDPF screening: {} total, {} screened (no violations), {} critical → NR",
        contingencies.len(),
        screened_count,
        critical_count
    );

    // Pass 2: Full NR on critical contingencies (parallel)
    if !critical_indices.is_empty() {
        let critical: Vec<&Contingency> = critical_indices
            .iter()
            .map(|&i| &contingencies[i])
            .collect();
        let nr_results = solve_contingencies_parallel(
            network,
            &critical,
            options,
            &base_case.voltage_magnitude_pu,
            &base_case.voltage_angle_rad,
        );
        for (idx, result) in critical_indices.iter().copied().zip(nr_results) {
            ordered_results[idx] = Some(result);
        }
    }

    Ok(ordered_results
        .into_iter()
        .map(|result| result.expect("every contingency should produce a result"))
        .collect())
}
