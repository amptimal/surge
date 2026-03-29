// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Prepared fixed-pattern NR solve for repeated studies on one network.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use surge_network::Network;
use surge_network::network::BusType;
use surge_solution::PfSolution;

use super::nr_bus_setup::{
    apply_angle_reference, apply_generator_p_limit_demotions, apply_generator_voltage_setpoints,
    apply_remote_reg_types, build_participation_map, build_remote_reg_map, build_zip_bus_data,
    classify_indices, dc_angle_init, reclassify_dead_pv_buses,
};
use super::nr_kernel::{
    AugmentedSlackData, NrKernelOptions, NrState, NrWorkspace, PreparedNrModel, ZipBusData,
    build_augmented_slack_data, run_nr_inner,
};
use super::nr_options::{AcPfError, AcPfOptions, StartupPolicy, WarmStart};
use super::nr_q_limits::build_nr_meta;
use super::nr_solve::{
    build_solution, nr_inner_iteration_limit, nr_inner_stall_limit, validate_branch_endpoints,
};
use crate::matrix::ybus::build_ybus;

/// Initialization mode for a prepared fixed-pattern NR solve.
#[derive(Clone, Debug)]
pub enum PreparedStart<'a> {
    /// Start from the network's stored voltage magnitudes and angles.
    CaseData,
    /// Start from Vm = 1.0 with the original slack angle preserved.
    Flat,
    /// Start from Vm = 1.0 with DC power flow angles.
    FlatDc,
    /// Start from a prior voltage state.
    Warm(&'a WarmStart),
}

/// Prepared fixed-pattern AC power flow solve.
///
/// This caches the expensive structures that are invariant across repeated
/// solves on the same network and bus-type pattern:
/// - Y-bus
/// - fused Jacobian sparsity pattern
/// - KLU symbolic analysis
/// - NR scratch workspace
///
/// The first implementation is intentionally narrow. It supports the fast
/// single-pattern NR path used for repeated warm-start studies and rejects
/// option combinations that require outer loops or dynamic bus-type changes.
pub struct PreparedAcPf {
    network: Arc<Network>,
    options: AcPfOptions,
    ybus: crate::matrix::ybus::YBus,
    fused_pattern: crate::matrix::fused::FusedPattern,
    klu: surge_sparse::KluSolver,
    workspace: NrWorkspace,
    p_spec_base: Vec<f64>,
    q_spec_base: Vec<f64>,
    zip_bus_data: Vec<ZipBusData>,
    participation: Option<HashMap<usize, f64>>,
    pvpq_indices: Vec<usize>,
    pq_indices: Vec<usize>,
    aug: Option<AugmentedSlackData>,
    bus_numbers: Vec<u32>,
    case_vm: Vec<f64>,
    case_va: Vec<f64>,
    flat_vm: Vec<f64>,
    flat_va: Vec<f64>,
    flat_dc_va: Option<Vec<f64>>,
    vm: Vec<f64>,
    va: Vec<f64>,
    orig_ref_idx: usize,
    convergence_history: Vec<(u32, f64)>,
}

pub(crate) fn validate_prepared_options(
    network: &Network,
    options: &AcPfOptions,
) -> Result<(), AcPfError> {
    if options.enforce_q_limits {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support Q-limit switching yet".to_string(),
        ));
    }
    if options.startup_policy != StartupPolicy::Single {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf requires startup_policy=single".to_string(),
        ));
    }
    if options.detect_islands {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf requires detect_islands=false".to_string(),
        ));
    }
    if options.auto_merge_zero_impedance {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf requires auto_merge_zero_impedance=false".to_string(),
        ));
    }
    if options.auto_reduce_topology {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf requires auto_reduce_topology=false".to_string(),
        ));
    }
    if options.enforce_interchange {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support interchange outer loops".to_string(),
        ));
    }
    if options.shunt_enabled
        && (!options.switched_shunts.is_empty() || !network.controls.switched_shunts.is_empty())
    {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support switched-shunt outer loops".to_string(),
        ));
    }
    if options.oltc_enabled && !options.oltc_controls.is_empty() {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support OLTC outer loops".to_string(),
        ));
    }
    if options.par_enabled && !options.par_controls.is_empty() {
        return Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support phase-angle regulator outer loops".to_string(),
        ));
    }
    Ok(())
}

impl PreparedAcPf {
    /// Build a prepared fixed-pattern NR solve for repeated studies on one network.
    pub fn new(network: Arc<Network>, options: &AcPfOptions) -> Result<Self, AcPfError> {
        let network =
            match crate::ac_dc::prepare_fixed_pattern_ac_network(network.as_ref(), options)? {
                std::borrow::Cow::Borrowed(_) => network,
                std::borrow::Cow::Owned(prepared) => Arc::new(prepared),
            };
        validate_prepared_options(&network, options)?;

        let n = network.n_buses();
        if n == 0 {
            return Err(AcPfError::EmptyNetwork);
        }
        if network.slack_bus_index().is_none() {
            return Err(AcPfError::NoSlackBus);
        }
        validate_branch_endpoints(&network)?;

        let bus_map = network.bus_index_map();
        let ybus = build_ybus(&network);
        let mut bus_types: Vec<BusType> = network.buses.iter().map(|b| b.bus_type).collect();

        if options.enforce_gen_p_limits {
            apply_generator_p_limit_demotions(&network, &mut bus_types);
        }

        let remote_reg_map = build_remote_reg_map(&network, &bus_map, &bus_types);
        let mut switched_to_pq = vec![false; n];
        reclassify_dead_pv_buses(&network, &mut bus_types, &mut switched_to_pq);

        let mut case_vm: Vec<f64> = network
            .buses
            .iter()
            .map(|b| b.voltage_magnitude_pu)
            .collect();
        let case_va: Vec<f64> = network.buses.iter().map(|b| b.voltage_angle_rad).collect();
        apply_generator_voltage_setpoints(&network, &bus_map, &bus_types, &mut case_vm);
        apply_remote_reg_types(
            &remote_reg_map,
            &mut bus_types,
            &mut case_vm,
            &mut switched_to_pq,
            None,
        );

        let mut flat_vm = vec![1.0; n];
        let mut flat_va = vec![0.0; n];
        for (i, bus) in network.buses.iter().enumerate() {
            if bus.bus_type == BusType::Slack {
                flat_va[i] = bus.voltage_angle_rad;
            }
        }
        apply_generator_voltage_setpoints(&network, &bus_map, &bus_types, &mut flat_vm);
        let mut flat_switched_to_pq = vec![false; n];
        let mut flat_bus_types = bus_types.clone();
        apply_remote_reg_types(
            &remote_reg_map,
            &mut flat_bus_types,
            &mut flat_vm,
            &mut flat_switched_to_pq,
            None,
        );
        let flat_dc_va = if options.dc_warm_start {
            dc_angle_init(&network)
        } else {
            None
        };

        let p_spec_base = network.bus_p_injection_pu();
        let q_spec_base = network.bus_q_injection_pu();
        let zip_bus_data = build_zip_bus_data(&network);
        let participation = build_participation_map(&network, options);
        let (pvpq_indices, pq_indices) = classify_indices(&bus_types);
        let fused_pattern =
            crate::matrix::fused::FusedPattern::new(&ybus, &pvpq_indices, &pq_indices);
        let dim = fused_pattern.dim();
        let symbolic_ref = fused_pattern.symbolic().as_ref();
        let col_ptrs: Vec<usize> = symbolic_ref.col_ptr().to_vec();
        let row_indices_klu: Vec<usize> = symbolic_ref.row_idx().to_vec();
        let klu = surge_sparse::KluSolver::new(dim, &col_ptrs, &row_indices_klu)
            .map_err(|e| AcPfError::InvalidNetwork(format!("KLU symbolic analysis failed: {e}")))?;
        let mut workspace = NrWorkspace::new(n, !zip_bus_data.is_empty());
        workspace.prepare_factor_buffers(fused_pattern.nnz(), dim);
        let aug = participation.as_ref().map(|pmap| {
            build_augmented_slack_data(&bus_types, pmap, &pvpq_indices, &pq_indices, dim)
        });
        let bus_numbers = network.buses.iter().map(|b| b.number).collect();
        let orig_ref_idx = bus_types
            .iter()
            .position(|t| *t == BusType::Slack)
            .unwrap_or(0);

        Ok(Self {
            network,
            options: options.clone(),
            ybus,
            fused_pattern,
            klu,
            workspace,
            p_spec_base,
            q_spec_base,
            zip_bus_data,
            participation,
            pvpq_indices,
            pq_indices,
            aug,
            bus_numbers,
            case_vm,
            case_va,
            flat_vm,
            flat_va,
            flat_dc_va,
            vm: vec![0.0; n],
            va: vec![0.0; n],
            orig_ref_idx,
            convergence_history: Vec::new(),
        })
    }

    /// Solve using the startup settings stored in `AcPfOptions`.
    pub fn solve(&mut self) -> Result<PfSolution, AcPfError> {
        if let Some(warm) = self.options.warm_start.clone() {
            return self.solve_with_start(PreparedStart::Warm(&warm));
        }
        if self.options.flat_start && self.options.dc_warm_start {
            return self.solve_with_start(PreparedStart::FlatDc);
        }
        if self.options.flat_start {
            return self.solve_with_start(PreparedStart::Flat);
        }
        self.solve_with_start(PreparedStart::CaseData)
    }

    /// Solve from an explicit prepared-start mode.
    pub fn solve_with_start(&mut self, start: PreparedStart<'_>) -> Result<PfSolution, AcPfError> {
        let solve_start = Instant::now();
        self.initialize_state(start)?;
        let va_ref0 = self.va[self.orig_ref_idx];
        let mut lambda = 0.0;

        self.convergence_history.clear();
        let history = if self.options.record_convergence_history {
            Some(&mut self.convergence_history)
        } else {
            None
        };

        let inner = run_nr_inner(
            PreparedNrModel {
                ybus: &self.ybus,
                fused_pattern: &self.fused_pattern,
                p_spec_base: &self.p_spec_base,
                q_spec_base: &self.q_spec_base,
                zip_bus_data: &self.zip_bus_data,
                participation: self.participation.as_ref(),
                pvpq_indices: &self.pvpq_indices,
                pq_indices: &self.pq_indices,
                aug: self.aug.as_ref(),
                options: NrKernelOptions {
                    tolerance: self.options.tolerance,
                    max_iterations: nr_inner_iteration_limit(&self.options),
                    stall_limit: nr_inner_stall_limit(&self.options),
                    vm_min: self.options.vm_min,
                    vm_max: self.options.vm_max,
                    line_search: self.options.line_search,
                    allow_partial_nonconverged: false,
                },
            },
            NrState {
                vm: &mut self.vm,
                va: &mut self.va,
                lambda: &mut lambda,
            },
            &mut self.workspace,
            &mut self.klu,
            history,
        )
        .map_err(|failure| AcPfError::NotConverged {
            iterations: failure.iterations,
            max_mismatch: failure.max_mismatch,
            worst_bus: failure
                .worst_internal_idx
                .and_then(|idx| self.bus_numbers.get(idx).copied()),
            partial_vm: Some(self.vm.clone()),
            partial_va: Some(self.va.clone()),
        })?;

        let solve_time = solve_start.elapsed().as_secs_f64();
        apply_angle_reference(
            &mut self.va,
            self.orig_ref_idx,
            va_ref0,
            self.options.angle_reference,
            &self.network,
        );
        let switched_to_pq = vec![false; self.network.n_buses()];
        let meta = build_nr_meta(
            &self.network,
            &self.bus_numbers,
            &switched_to_pq,
            0,
            &self.participation,
            lambda,
            &self.options,
        );

        Ok(build_solution(
            &self.network,
            &self.vm,
            &self.va,
            self.workspace.p_calc(),
            self.workspace.q_calc(),
            inner.iterations,
            inner.max_mismatch,
            solve_time,
            self.bus_numbers.clone(),
            meta,
            self.convergence_history.clone(),
        ))
    }

    fn initialize_state(&mut self, start: PreparedStart<'_>) -> Result<(), AcPfError> {
        match start {
            PreparedStart::CaseData => {
                self.vm.copy_from_slice(&self.case_vm);
                self.va.copy_from_slice(&self.case_va);
            }
            PreparedStart::Flat => {
                self.vm.copy_from_slice(&self.flat_vm);
                self.va.copy_from_slice(&self.flat_va);
            }
            PreparedStart::FlatDc => {
                self.vm.copy_from_slice(&self.flat_vm);
                if let Some(dc_va) = self.flat_dc_va.as_ref() {
                    self.va.copy_from_slice(dc_va);
                } else {
                    self.va.copy_from_slice(&self.flat_va);
                }
            }
            PreparedStart::Warm(prior) => {
                let n = self.network.n_buses();
                if prior.vm.len() != n || prior.va.len() != n {
                    return Err(AcPfError::InvalidOptions(format!(
                        "warm start has {} Vm entries and {} Va entries, expected {n}",
                        prior.vm.len(),
                        prior.va.len()
                    )));
                }
                self.vm.copy_from_slice(&prior.vm);
                self.va.copy_from_slice(&prior.va);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::nr_options::DcLineModel;
    use surge_network::network::{
        Branch, Bus, BusType, Generator, LccConverterTerminal, LccHvdcControlMode, LccHvdcLink,
        Load,
    };

    fn make_3bus_base() -> Network {
        let mut net = Network::new("prepared-3bus");
        net.buses.extend([
            Bus::new(1, BusType::Slack, 345.0),
            Bus::new(2, BusType::PQ, 345.0),
            Bus::new(3, BusType::PV, 345.0),
        ]);
        net.loads.push(Load::new(2, 200.0, 0.0));
        net.loads.push(Load::new(3, 100.0, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.005, 0.05, 0.04));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.02));
        net.generators.push(Generator::new(1, 300.0, 1.0));
        net.generators.push(Generator::new(3, 100.0, 1.0));
        net
    }

    fn prepared_options() -> AcPfOptions {
        AcPfOptions {
            detect_islands: false,
            auto_merge_zero_impedance: false,
            auto_reduce_topology: false,
            enforce_q_limits: false,
            oltc_enabled: false,
            par_enabled: false,
            shunt_enabled: false,
            enforce_interchange: false,
            startup_policy: StartupPolicy::Single,
            ..Default::default()
        }
    }

    #[test]
    fn prepared_ac_pf_uses_same_fixed_schedule_hvdc_model_as_public_solver() {
        let mut net = make_3bus_base();
        net.hvdc.push_lcc_link(LccHvdcLink {
            name: "DC1".into(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 1.0,
            scheduled_setpoint: 100.0,
            scheduled_voltage_kv: 345.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 5.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 3,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 17.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        });

        let opts = prepared_options();
        let expected = crate::ac_dc::solve_ac_pf_with_dc_lines(&net, &opts).unwrap();
        let mut prepared = PreparedAcPf::new(Arc::new(net), &opts).unwrap();
        let actual = prepared.solve().unwrap();

        assert_eq!(
            actual.voltage_magnitude_pu.len(),
            expected.voltage_magnitude_pu.len()
        );
        for (lhs, rhs) in actual
            .voltage_magnitude_pu
            .iter()
            .zip(expected.voltage_magnitude_pu.iter())
        {
            assert!((lhs - rhs).abs() < 1e-10, "Vm mismatch: {lhs} vs {rhs}");
        }
        for (lhs, rhs) in actual
            .voltage_angle_rad
            .iter()
            .zip(expected.voltage_angle_rad.iter())
        {
            assert!((lhs - rhs).abs() < 1e-10, "Va mismatch: {lhs} vs {rhs}");
        }
    }

    #[test]
    fn prepared_ac_pf_rejects_sequential_ac_dc_models() {
        let mut net = make_3bus_base();
        net.hvdc.push_lcc_link(LccHvdcLink {
            name: "DC1".into(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            scheduled_setpoint: 100.0,
            scheduled_voltage_kv: 345.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 5.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 3,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 17.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        });

        let mut opts = prepared_options();
        opts.dc_line_model = DcLineModel::SequentialAcDc;

        let err = PreparedAcPf::new(Arc::new(net), &opts)
            .err()
            .expect("Sequential AC/DC prepared solve should be rejected");
        match err {
            AcPfError::InvalidOptions(message) => {
                assert!(
                    message.contains("SequentialAcDc"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected InvalidOptions, got {other:?}"),
        }
    }
}
