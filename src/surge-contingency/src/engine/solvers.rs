// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Full-clone and fast-path contingency solvers.

use surge_ac::NrWorkspace;
use surge_ac::matrix::fused::FusedPattern;
use surge_ac::matrix::ybus::{YBus, build_ybus_from_parts};
use surge_network::Network;
use surge_network::network::BusType;
use surge_network::network::Contingency;
use surge_solution::SolveStatus;
use surge_sparse::KluSolver;
use tracing::warn;

use super::islands::solve_post_network_with_island_detection;
use super::parallel::{non_converged_result, solve_acpf_inline};
use super::power_flow::{network_has_hvdc_assets, solve_network_pf_with_fallback};
use crate::types::{
    ContingencyOptions, ContingencyResult, ContingencyStatus, Violation, VoltageStressMode,
};
use crate::violations::{compute_branch_flows_mva, detect_violations};
use crate::voltage::stress_proxy::compute_voltage_stress_summary;

/// Fallback: clone the full Network for contingencies with generator outages,
/// HVDC trips, modifications, or breaker operations.
pub(crate) fn solve_contingency_full_clone(
    network: &Network,
    ctg: &Contingency,
    options: &ContingencyOptions,
) -> ContingencyResult {
    let mut net = network.clone();

    for &br_idx in &ctg.branch_indices {
        if br_idx < net.branches.len() {
            net.branches[br_idx].in_service = false;
        }
    }
    for &gen_idx in &ctg.generator_indices {
        if gen_idx < net.generators.len() {
            net.generators[gen_idx].in_service = false;
        }
    }
    // Block HVDC links referenced by converter/cable indices.
    if !ctg.hvdc_converter_indices.is_empty() || !ctg.hvdc_cable_indices.is_empty() {
        let hvdc_links = surge_hvdc::interop::links_from_network(&net);
        for &hvdc_idx in ctg
            .hvdc_converter_indices
            .iter()
            .chain(ctg.hvdc_cable_indices.iter())
        {
            if hvdc_idx >= hvdc_links.len() {
                continue;
            }
            let link_name = hvdc_links[hvdc_idx].name();
            for link in &mut net.hvdc.links {
                if let Some(dc) = link.as_lcc_mut()
                    && dc.name == link_name
                {
                    dc.mode = surge_network::network::LccHvdcControlMode::Blocked;
                }
                if let Some(vsc) = link.as_vsc_mut()
                    && vsc.name == link_name
                {
                    vsc.mode = surge_network::network::VscHvdcControlMode::Blocked;
                }
            }
        }
    }

    // Apply simultaneous network modifications (PSS/E .con SET/CHANGE commands).
    if let Err(err) =
        surge_network::network::apply_contingency_modifications(&mut net, &ctg.modifications)
    {
        warn!(
            contingency = %ctg.id,
            error = %err,
            "contingency modification failed; treating contingency as non-converged"
        );
        return ContingencyResult {
            id: ctg.id.clone(),
            label: ctg.label.clone(),
            branch_indices: ctg.branch_indices.clone(),
            generator_indices: ctg.generator_indices.clone(),
            status: ContingencyStatus::NonConverged,
            converged: false,
            violations: vec![Violation::NonConvergent {
                max_mismatch: f64::INFINITY,
                iterations: 0,
            }],
            tpl_category: ctg.tpl_category,
            ..Default::default()
        };
    }

    // Apply breaker switch operations and rebuild_topology the network.
    if !ctg.switch_ids.is_empty() {
        let mut topology_changed = false;
        if let Some(sm) = net.topology.as_mut() {
            for sw_id in &ctg.switch_ids {
                topology_changed |= sm.set_switch_state(sw_id, true); // open the breaker
            }
        }
        if topology_changed {
            match surge_topology::rebuild_topology(&net) {
                Ok(retopo) => net = retopo,
                Err(_) => {
                    return ContingencyResult {
                        id: ctg.id.clone(),
                        label: ctg.label.clone(),
                        branch_indices: ctg.branch_indices.clone(),
                        generator_indices: ctg.generator_indices.clone(),
                        status: ContingencyStatus::NonConverged,
                        tpl_category: ctg.tpl_category,
                        ..Default::default()
                    };
                }
            }
        }
    }

    if options.detect_islands
        && !network_has_hvdc_assets(&net)
        && let Some(result) = solve_post_network_with_island_detection(&net, ctg, options)
    {
        return result;
    }

    let solve_options = surge_ac::AcPfOptions {
        flat_start: options.contingency_flat_start,
        ..options.acpf_options.clone()
    };

    match solve_network_pf_with_fallback(&net, &solve_options) {
        Ok(sol) if sol.status == SolveStatus::Converged => {
            let violations = detect_violations(&net, &sol, options);
            let bus_map_fc = net.bus_index_map();
            let ybus_fc = build_ybus_from_parts(
                &net.branches,
                &net.buses,
                net.base_mva,
                &bus_map_fc,
                &net.metadata.impedance_corrections,
            );
            let voltage_stress = compute_voltage_stress_summary(
                &net,
                &ybus_fc,
                &sol.voltage_magnitude_pu,
                &sol.voltage_angle_rad,
                &sol.reactive_power_injection_pu,
                &options.voltage_stress_mode,
            );
            let (pvm, pva, pflows) = if options.store_post_voltages {
                (
                    Some(sol.voltage_magnitude_pu.clone()),
                    Some(sol.voltage_angle_rad.clone()),
                    Some(compute_branch_flows_mva(
                        &net.branches,
                        net.base_mva,
                        &bus_map_fc,
                        None,
                        &sol.voltage_magnitude_pu,
                        &sol.voltage_angle_rad,
                    )),
                )
            } else {
                (None, None, None)
            };
            ContingencyResult {
                id: ctg.id.clone(),
                label: ctg.label.clone(),
                branch_indices: ctg.branch_indices.clone(),
                generator_indices: ctg.generator_indices.clone(),
                status: ContingencyStatus::Converged,
                converged: true,
                iterations: sol.iterations,
                violations,
                n_islands: 1,
                voltage_stress: voltage_stress.into_option(),
                post_vm: pvm,
                post_va: pva,
                post_branch_flows: pflows,
                tpl_category: ctg.tpl_category,
                ..Default::default()
            }
        }
        Ok(sol) => ContingencyResult {
            id: ctg.id.clone(),
            label: ctg.label.clone(),
            branch_indices: ctg.branch_indices.clone(),
            generator_indices: ctg.generator_indices.clone(),
            status: ContingencyStatus::NonConverged,
            iterations: sol.iterations,
            violations: vec![Violation::NonConvergent {
                max_mismatch: sol.max_mismatch,
                iterations: sol.iterations,
            }],
            tpl_category: ctg.tpl_category,
            ..Default::default()
        },
        Err(_) => ContingencyResult {
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
        },
    }
}

// ---------------------------------------------------------------------------
// CTG-01: Fast generator N-1 path
// ---------------------------------------------------------------------------

/// Solve a generator-only contingency without recomputing the Y-bus (CTG-01).
///
/// Generator outages change only the power injection vectors and possibly the
/// bus classification (PV → PQ when no other generator remains on that bus).
/// The Y-bus admittance matrix is invariant under generator outages, so we
/// reuse the base Y-bus directly.
#[allow(clippy::too_many_arguments)]
pub(crate) fn solve_generator_contingency_fast(
    network: &Network,
    ctg: &Contingency,
    base_ybus: &YBus,
    base_vm: &[f64],
    base_va: &[f64],
    options: &ContingencyOptions,
) -> ContingencyResult {
    let bus_map = network.bus_index_map();
    let n = network.n_buses();

    // Start with base injection vectors.
    let mut p_spec = network.bus_p_injection_pu();
    let mut q_spec = network.bus_q_injection_pu();

    // Track which bus indices are affected by tripped generators.
    let mut affected_bus_indices: std::collections::HashSet<usize> = Default::default();

    for &gen_idx in &ctg.generator_indices {
        let Some(generator) = network.generators.get(gen_idx) else {
            continue;
        };
        if !generator.in_service {
            continue;
        }
        let Some(&bus_idx) = bus_map.get(&generator.bus) else {
            continue;
        };
        // Remove this generator's contribution from the injection vectors.
        p_spec[bus_idx] -= generator.p / network.base_mva;
        q_spec[bus_idx] -= generator.q / network.base_mva;
        affected_bus_indices.insert(bus_idx);
    }

    // Build the set of tripped generator indices for fast membership testing.
    let tripped: std::collections::HashSet<usize> = ctg.generator_indices.iter().cloned().collect();

    // Determine post-outage bus types.
    let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();
    {
        let mut live_gen_count = vec![0u32; n];
        for g in &network.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
            {
                live_gen_count[idx] += 1;
            }
        }
        for (i, bt) in bus_types.iter_mut().enumerate() {
            if *bt == BusType::PV && live_gen_count[i] == 0 {
                *bt = BusType::PQ;
            }
        }
    }
    for &bus_idx in &affected_bus_indices {
        if bus_types[bus_idx] != BusType::PV {
            continue;
        }
        let has_remaining = network.generators.iter().enumerate().any(|(gi, g)| {
            g.in_service && !tripped.contains(&gi) && {
                let Some(&bidx) = bus_map.get(&g.bus) else {
                    return false;
                };
                bidx == bus_idx
            }
        });
        if !has_remaining {
            bus_types[bus_idx] = BusType::PQ;
        }
    }

    // P1-011: Enforce reactive power limits on remaining generators after redispatch.
    for &bus_idx in &affected_bus_indices {
        if bus_types[bus_idx] != BusType::PV {
            continue;
        }
        let (mut agg_qmin, mut agg_qmax) = (0.0_f64, 0.0_f64);
        for (gi, g) in network.generators.iter().enumerate() {
            if !g.in_service || tripped.contains(&gi) {
                continue;
            }
            let Some(&bidx) = bus_map.get(&g.bus) else {
                continue;
            };
            if bidx == bus_idx {
                agg_qmin += g.qmin;
                agg_qmax += g.qmax;
            }
        }
        let bus_qd_mvar = network.bus_load_q_mvar();
        let q_load_pu = bus_qd_mvar[bus_idx] / network.base_mva;
        let q_required_mvar = (q_spec[bus_idx] + q_load_pu) * network.base_mva;
        if q_required_mvar > agg_qmax {
            warn!(
                "P1-011: Bus {} Q required {:.2} MVAr exceeds Qmax {:.2} MVAr after gen trip — demoting to PQ",
                network.buses[bus_idx].number, q_required_mvar, agg_qmax
            );
            bus_types[bus_idx] = BusType::PQ;
            q_spec[bus_idx] = agg_qmax / network.base_mva - q_load_pu;
        } else if q_required_mvar < agg_qmin {
            warn!(
                "P1-011: Bus {} Q required {:.2} MVAr below Qmin {:.2} MVAr after gen trip — demoting to PQ",
                network.buses[bus_idx].number, q_required_mvar, agg_qmin
            );
            bus_types[bus_idx] = BusType::PQ;
            q_spec[bus_idx] = agg_qmin / network.base_mva - q_load_pu;
        }
    }

    // Rebuild pvpq / pq indices from the modified bus types.
    let mut pv_indices: Vec<usize> = Vec::new();
    let mut pq_indices: Vec<usize> = Vec::new();
    for (i, bt) in bus_types.iter().enumerate() {
        match bt {
            BusType::PV => pv_indices.push(i),
            BusType::PQ => pq_indices.push(i),
            _ => {}
        }
    }
    let mut pvpq_indices: Vec<usize> = pv_indices.clone();
    pvpq_indices.extend(&pq_indices);
    pvpq_indices.sort();

    // Build a new FusedPattern for the (possibly changed) pvpq/pq sets.
    let fused_pattern = FusedPattern::new(base_ybus, &pvpq_indices, &pq_indices);
    let dim = fused_pattern.dim();
    let nnz = fused_pattern.nnz();

    let symbolic_ref = fused_pattern.symbolic().as_ref();
    let col_ptrs: Vec<usize> = symbolic_ref.col_ptr().to_vec();
    let row_indices: Vec<usize> = symbolic_ref.row_idx().to_vec();

    let mut klu = match KluSolver::new(dim, &col_ptrs, &row_indices) {
        Ok(k) => k,
        Err(_) => {
            let mut r = non_converged_result(f64::INFINITY, 0);
            r.id = ctg.id.clone();
            r.label = ctg.label.clone();
            r.branch_indices = ctg.branch_indices.clone();
            r.generator_indices = ctg.generator_indices.clone();
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
    let post_network = if matches!(
        options.voltage_stress_mode,
        VoltageStressMode::ExactLIndex { .. }
    ) {
        let mut net = network.clone();
        for &gen_idx in &ctg.generator_indices {
            if gen_idx < net.generators.len() {
                net.generators[gen_idx].in_service = false;
            }
        }
        Some(net)
    } else {
        None
    };

    let inner = solve_acpf_inline(
        &fused_pattern,
        base_ybus,
        post_network.as_ref(),
        &mut klu,
        &mut workspace,
        &mut vm,
        &mut va,
        &p_spec,
        &q_spec,
        &network.flowgates,
        &network.interfaces,
        None,
        &pvpq_indices,
        &pq_indices,
        &network.branches,
        &network.buses,
        network.base_mva,
        &bus_map,
        options,
    );

    let mut result = inner;
    result.id = ctg.id.clone();
    result.label = ctg.label.clone();
    result.branch_indices = ctg.branch_indices.clone();
    result.generator_indices = ctg.generator_indices.clone();
    result.tpl_category = ctg.tpl_category;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
    use surge_network::network::{Branch, Bus, BusType, Contingency, Generator, Load};

    fn test_network() -> Network {
        let mut net = Network::new("generator_fast_path");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));

        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        branch.rating_a_mva = 100.0;
        net.branches.push(branch);

        net.generators.push(Generator::new(1, 100.0, 1.0));
        net.loads.push(Load::new(2, 50.0, 0.0));
        net
    }

    #[test]
    fn generator_fast_path_preserves_nonconverged_status() {
        let net = test_network();
        let base = solve_ac_pf_kernel(&net, &AcPfOptions::default())
            .expect("base AC solve should converge");
        assert_eq!(base.status, SolveStatus::Converged);

        let ctg = Contingency {
            id: "gen_trip".into(),
            label: "generator trip".into(),
            generator_indices: vec![0],
            ..Default::default()
        };
        let mut options = ContingencyOptions::default();
        options.acpf_options.max_iterations = 0;
        options.contingency_flat_start = true;

        let result = solve_generator_contingency_fast(
            &net,
            &ctg,
            &build_ybus_from_parts(
                &net.branches,
                &net.buses,
                net.base_mva,
                &net.bus_index_map(),
                &net.metadata.impedance_corrections,
            ),
            &base.voltage_magnitude_pu,
            &base.voltage_angle_rad,
            &options,
        );

        assert_eq!(result.status, ContingencyStatus::NonConverged);
        assert!(!result.converged);
    }
}
