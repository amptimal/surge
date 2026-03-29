// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Parallel AC contingency solver using rayon with thread-local pooling.

use std::collections::{HashMap, HashSet};
use std::panic;
use std::sync::Mutex;

use rayon::prelude::*;
use surge_ac::matrix::fused::FusedPattern;
use surge_ac::matrix::ybus::{YBus, build_ybus_from_parts};
use surge_ac::{FdpfFactors, NrKernelOptions, NrState, NrWorkspace, PreparedNrModel, run_nr_inner};
use surge_network::Network;
use surge_network::network::BusType;
use surge_network::network::{Branch, Bus, Contingency, Flowgate, Interface};
use surge_sparse::KluSolver;
use tracing::warn;

use super::islands::{find_connected_components, solve_with_island_detection};
use super::power_flow::network_has_hvdc_assets;
use super::solvers::{solve_contingency_full_clone, solve_generator_contingency_fast};
use crate::types::{
    ContingencyOptions, ContingencyResult, ContingencyStatus, Violation, VoltageStressMode,
};
use crate::violations::detect_violations_from_parts;
use crate::voltage::stress_proxy::{
    VoltageStressSummary, classify_l_index_category, compute_exact_voltage_stress,
    compute_voltage_stress_proxy,
};

fn is_pure_generator_outage(ctg: &Contingency) -> bool {
    !ctg.generator_indices.is_empty()
        && ctg.branch_indices.is_empty()
        && ctg.hvdc_converter_indices.is_empty()
        && ctg.hvdc_cable_indices.is_empty()
        && ctg.switch_ids.is_empty()
        && ctg.modifications.is_empty()
}

/// Build a non-converged ContingencyResult (shared by all early-exit paths).
pub(crate) fn non_converged_result(max_mismatch: f64, iterations: u32) -> ContingencyResult {
    ContingencyResult {
        status: ContingencyStatus::NonConverged,
        iterations,
        violations: vec![Violation::NonConvergent {
            max_mismatch,
            iterations,
        }],
        ..Default::default()
    }
}

/// Inline NR solve using pre-allocated thread-local buffers.
///
/// Returns a partial ContingencyResult (id/label are set by caller).
#[allow(clippy::too_many_arguments)]
pub(crate) fn solve_acpf_inline(
    fused_pattern: &FusedPattern,
    ybus: &YBus,
    post_network: Option<&Network>,
    klu: &mut KluSolver,
    workspace: &mut NrWorkspace,
    vm: &mut [f64],
    va: &mut [f64],
    p_spec: &[f64],
    q_spec: &[f64],
    flowgates: &[Flowgate],
    interfaces: &[Interface],
    outaged_branches: Option<&HashSet<usize>>,
    pvpq_indices: &[usize],
    pq_indices: &[usize],
    branches: &[Branch],
    buses: &[Bus],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    options: &ContingencyOptions,
) -> ContingencyResult {
    let mut lambda = 0.0_f64;
    let inner = match run_nr_inner(
        PreparedNrModel {
            ybus,
            fused_pattern,
            p_spec_base: p_spec,
            q_spec_base: q_spec,
            zip_bus_data: &[],
            participation: None,
            pvpq_indices,
            pq_indices,
            aug: None,
            options: NrKernelOptions {
                tolerance: options.acpf_options.tolerance,
                max_iterations: options.acpf_options.max_iterations,
                stall_limit: options.acpf_options.inner_stall_limit(),
                vm_min: options.acpf_options.vm_min,
                vm_max: options.acpf_options.vm_max,
                line_search: options.acpf_options.line_search,
                allow_partial_nonconverged: false,
            },
        },
        NrState {
            vm,
            va,
            lambda: &mut lambda,
        },
        workspace,
        klu,
        None,
    ) {
        Ok(result) if result.converged => result,
        Ok(result) => return non_converged_result(result.max_mismatch, result.iterations),
        Err(failure) => return non_converged_result(failure.max_mismatch, failure.iterations),
    };

    let violations = detect_violations_from_parts(
        branches,
        buses,
        base_mva,
        bus_map,
        outaged_branches,
        vm,
        va,
        options,
        flowgates,
        interfaces,
    );
    let voltage_stress = match options.voltage_stress_mode {
        VoltageStressMode::Off => VoltageStressSummary::default(),
        VoltageStressMode::Proxy => {
            compute_voltage_stress_proxy(buses, ybus, vm, workspace.q_calc())
        }
        VoltageStressMode::ExactLIndex { l_index_threshold } => {
            let mut summary = compute_exact_voltage_stress(
                post_network.expect("exact contingency voltage stress requires post network"),
                vm,
                va,
            );
            summary.vsm_category = summary
                .max_exact_l_index
                .map(|val| classify_l_index_category(val, l_index_threshold));
            summary
        }
    };
    let (pvm, pva, pflows) = if options.store_post_voltages {
        (
            Some(vm.to_vec()),
            Some(va.to_vec()),
            Some(crate::violations::compute_branch_flows_mva(
                branches,
                base_mva,
                bus_map,
                outaged_branches,
                vm,
                va,
            )),
        )
    } else {
        (None, None, None)
    };
    ContingencyResult {
        status: ContingencyStatus::Converged,
        converged: true,
        iterations: inner.iterations,
        violations,
        n_islands: 1,
        voltage_stress: voltage_stress.into_option(),
        post_vm: pvm,
        post_va: pva,
        post_branch_flows: pflows,
        ..Default::default()
    }
}

/// Solve contingencies in parallel using rayon with thread-local pooling.
///
/// Optimizations vs naive approach (cumulative ~15x speedup):
/// 1. **Warm start**: Base case voltages → 2-3 NR iterations instead of 5-8
/// 2. **Thread-local Y-bus pool**: Clone Y-bus once per thread, then apply/unapply
///    deltas per contingency (saves n_ctg × 10MB allocations on large cases)
/// 3. **Thread-local KLU reuse**: KLU symbolic analysis once per thread, only
///    numeric factor/refactor per contingency
/// 4. **Thread-local working arrays**: vm, va, p_calc, q_calc, csc_values, rhs
///    allocated once per thread, reset via memcpy per contingency
/// 5. **FusedPattern reuse**: Build structural mapping once, shared across threads
/// 6. **catch_unwind**: Prevents thread panics from killing rayon pool
pub(crate) fn solve_contingencies_parallel(
    network: &Network,
    contingencies: &[&Contingency],
    options: &ContingencyOptions,
    base_vm: &[f64],
    base_va: &[f64],
) -> Vec<ContingencyResult> {
    let network_has_hvdc = network_has_hvdc_assets(network);

    // Pre-compute shared data once (invariant across branch-only contingencies)
    let bus_map = network.bus_index_map();
    let n = network.n_buses();

    // Reclassify PV buses with no in-service generators as PQ
    let mut live_gen_count = vec![0u32; n];
    for g in &network.generators {
        if g.in_service
            && let Some(&idx) = bus_map.get(&g.bus)
        {
            live_gen_count[idx] += 1;
        }
    }
    let mut pv_indices: Vec<usize> = Vec::new();
    let mut pq_indices: Vec<usize> = Vec::new();
    for (i, bus) in network.buses.iter().enumerate() {
        match bus.bus_type {
            BusType::PV => {
                if live_gen_count[i] > 0 {
                    pv_indices.push(i);
                } else {
                    pq_indices.push(i);
                }
            }
            BusType::PQ => pq_indices.push(i),
            _ => {}
        }
    }
    let mut pvpq_indices: Vec<usize> = Vec::new();
    pvpq_indices.extend(&pv_indices);
    pvpq_indices.extend(&pq_indices);
    pvpq_indices.sort();
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

    // Build FusedPattern ONCE from base case — reused across ALL contingencies.
    let fused_pattern = FusedPattern::new(&base_ybus, &pvpq_indices, &pq_indices);
    let dim = fused_pattern.dim();
    let nnz = fused_pattern.nnz();

    // Extract CSC structure for KLU symbolic analysis
    let symbolic_ref = fused_pattern.symbolic().as_ref();
    let col_ptrs: Vec<usize> = symbolic_ref.col_ptr().to_vec();
    let row_indices: Vec<usize> = symbolic_ref.row_idx().to_vec();

    // Impedance correction map
    let corr_map: HashMap<
        u32,
        &surge_network::network::impedance_correction::ImpedanceCorrectionTable,
    > = network
        .metadata
        .impedance_corrections
        .iter()
        .map(|t| (t.number, t))
        .collect();

    // Pre-compute branch removal deltas for all contingencies
    let branch_deltas =
        super::compute_branch_deltas(contingencies.iter().copied(), network, &bus_map, &corr_map);

    // B8-1d: FDPF fallback pool
    let fallback_fdpf_pool: Mutex<Vec<FdpfFactors>> = Mutex::new(Vec::new());

    // Progress tracking
    let progress_counter = std::sync::atomic::AtomicUsize::new(0);
    let total_ctgs = contingencies.len();

    contingencies
        .par_iter()
        .zip(branch_deltas.par_iter())
        .map(|(ctg, deltas_opt)| {
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                if network_has_hvdc {
                    solve_contingency_full_clone(network, ctg, options)
                } else if let Some(deltas) = deltas_opt {
                    // Branch-only path — check for islands first (CTG-09).
                    if options.detect_islands && !ctg.branch_indices.is_empty() {
                        let (_, n_comp) = find_connected_components(network, &ctg.branch_indices);
                        if n_comp > 1 {
                            let mut result = solve_with_island_detection(
                                network, ctg, options, base_vm, base_va,
                            );
                            if let Some(ref mut flows) = result.post_branch_flows {
                                for &br_idx in &ctg.branch_indices {
                                    if br_idx < flows.len() {
                                        flows[br_idx] = 0.0;
                                    }
                                }
                            }
                            return result;
                        }
                    }

                    // No islands — standard inline path.
                    let mut ybus = base_ybus.clone();
                    for delta in deltas {
                        ybus.apply_deltas(delta);
                    }

                    let mut klu = match KluSolver::new(dim, &col_ptrs, &row_indices) {
                        Ok(k) => k,
                        Err(_) => {
                            let mut r = non_converged_result(f64::INFINITY, 0);
                            r.id = ctg.id.clone();
                            r.label = ctg.label.clone();
                            r.tpl_category = ctg.tpl_category;
                            return r;
                        }
                    };
                    let mut vm = if options.contingency_flat_start {
                        vec![1.0_f64; base_vm.len()]
                    } else {
                        base_vm.to_vec()
                    };
                    let mut va = if options.contingency_flat_start {
                        vec![0.0_f64; base_va.len()]
                    } else {
                        base_va.to_vec()
                    };
                    let mut workspace = NrWorkspace::new(n, false);
                    workspace.prepare_factor_buffers(nnz, dim);
                    let outaged: HashSet<usize> = ctg.branch_indices.iter().copied().collect();
                    let post_network = if matches!(
                        options.voltage_stress_mode,
                        VoltageStressMode::ExactLIndex { .. }
                    ) {
                        let mut net = network.clone();
                        for &br_idx in &ctg.branch_indices {
                            if br_idx < net.branches.len() {
                                net.branches[br_idx].in_service = false;
                            }
                        }
                        Some(net)
                    } else {
                        None
                    };

                    let inner = solve_acpf_inline(
                        &fused_pattern,
                        &ybus,
                        post_network.as_ref(),
                        &mut klu,
                        &mut workspace,
                        &mut vm,
                        &mut va,
                        &p_spec,
                        &q_spec,
                        &network.flowgates,
                        &network.interfaces,
                        Some(&outaged),
                        &pvpq_indices,
                        &pq_indices,
                        &network.branches,
                        &network.buses,
                        network.base_mva,
                        &bus_map,
                        options,
                    );
                    let mut result = ContingencyResult {
                        id: ctg.id.clone(),
                        label: ctg.label.clone(),
                        branch_indices: ctg.branch_indices.clone(),
                        generator_indices: ctg.generator_indices.clone(),
                        tpl_category: ctg.tpl_category,
                        ..inner
                    };

                    // B8-1d: FDPF fallback — when NR fails to converge
                    if !result.converged {
                        let fdpf_opt = match fallback_fdpf_pool
                            .lock()
                            .expect("fallback_fdpf_pool mutex should not be poisoned")
                            .pop()
                        {
                            Some(f) => Some(f),
                            None => FdpfFactors::new(network).ok(),
                        };

                        if let Some(mut fdpf) = fdpf_opt {
                            let fdpf_result = fdpf.solve_from_ybus(
                                &ybus,
                                &p_spec,
                                &q_spec,
                                base_vm,
                                base_va,
                                1e-4,
                                options.fdpf_max_iterations,
                            );

                            fallback_fdpf_pool
                                .lock()
                                .expect("fallback_fdpf_pool mutex should not be poisoned")
                                .push(fdpf);

                            if let Some(fb_r) = fdpf_result {
                                let fb_vm = fb_r.vm;
                                let fb_va = fb_r.va;
                                let violations = detect_violations_from_parts(
                                    &network.branches,
                                    &network.buses,
                                    network.base_mva,
                                    &bus_map,
                                    Some(&outaged),
                                    &fb_vm,
                                    &fb_va,
                                    options,
                                    &network.flowgates,
                                    &network.interfaces,
                                );

                                result.fdpf_fallback = true;
                                result.status = ContingencyStatus::Approximate;
                                if options.store_post_voltages {
                                    result.post_branch_flows =
                                        Some(crate::violations::compute_branch_flows_mva(
                                            &network.branches,
                                            network.base_mva,
                                            &bus_map,
                                            Some(&outaged),
                                            &fb_vm,
                                            &fb_va,
                                        ));
                                    result.post_vm = Some(fb_vm.clone());
                                    result.post_va = Some(fb_va.clone());
                                }
                                result.violations = violations;
                            }
                        }
                    }

                    result
                } else if is_pure_generator_outage(ctg) {
                    solve_generator_contingency_fast(
                        network, ctg, &base_ybus, base_vm, base_va, options,
                    )
                } else {
                    solve_contingency_full_clone(network, ctg, options)
                }
            }));

            let r = match result {
                Ok(r) => r,
                Err(e) => {
                    warn!("Contingency {} panicked: {:?}", ctg.id, e);
                    ContingencyResult {
                        id: ctg.id.clone(),
                        label: ctg.label.clone(),
                        branch_indices: ctg.branch_indices.clone(),
                        generator_indices: ctg.generator_indices.clone(),
                        status: ContingencyStatus::NonConverged,
                        violations: vec![Violation::NonConvergent {
                            max_mismatch: f64::INFINITY,
                            iterations: 0,
                        }],
                        tpl_category: ctg.tpl_category,
                        ..Default::default()
                    }
                }
            };

            // Invoke progress callback
            if let Some(ref cb) = options.progress_cb {
                let done = progress_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                (cb.0)(done, total_ctgs);
            }

            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::is_pure_generator_outage;
    use surge_network::network::Contingency;

    fn contingency() -> Contingency {
        Contingency {
            id: "ctg".into(),
            label: "ctg".into(),
            ..Default::default()
        }
    }

    #[test]
    fn pure_generator_outage_detects_generator_only_contingencies() {
        let mut ctg = contingency();
        ctg.generator_indices = vec![0];
        assert!(is_pure_generator_outage(&ctg));
    }

    #[test]
    fn pure_generator_outage_rejects_switch_contingencies() {
        let mut ctg = contingency();
        ctg.switch_ids = vec!["BRK_1".into()];
        assert!(!is_pure_generator_outage(&ctg));
    }

    #[test]
    fn pure_generator_outage_rejects_mixed_branch_generator_contingencies() {
        let mut ctg = contingency();
        ctg.branch_indices = vec![3];
        ctg.generator_indices = vec![1];
        assert!(!is_pure_generator_outage(&ctg));
    }
}
