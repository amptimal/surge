// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Voltage stress helper computations for post-contingency analysis.

use num_complex::Complex64;
use surge_ac::matrix::ybus::{YBus, build_ybus};
use surge_network::Network;
use surge_network::network::{Bus, BusType};
use surge_sparse::{ComplexKluSolver, CscMatrix};

use crate::types::{BusVoltageStress, VoltageStressMode, VsmCategory};

/// Classify a max L-index value into a voltage stability category.
pub(crate) fn classify_l_index_category(max_l_index: f64, critical_threshold: f64) -> VsmCategory {
    let critical_threshold = critical_threshold.clamp(0.5, 0.9);
    if max_l_index >= 0.9 {
        VsmCategory::Unstable
    } else if max_l_index >= critical_threshold {
        VsmCategory::Critical
    } else if max_l_index >= 0.5 {
        VsmCategory::Marginal
    } else {
        VsmCategory::Secure
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct VoltageStressSummary {
    pub voltage_stress: Vec<BusVoltageStress>,
    pub max_local_qv_stress_proxy: Option<f64>,
    pub critical_proxy_bus: Option<u32>,
    pub max_exact_l_index: Option<f64>,
    pub critical_exact_l_bus: Option<u32>,
    pub vsm_category: Option<VsmCategory>,
}

impl VoltageStressSummary {
    /// Convert into an `Option<VoltageStressResult>`.
    ///
    /// Returns `None` when the per-bus vec is empty (stress mode was `Off`).
    pub fn into_option(self) -> Option<crate::types::VoltageStressResult> {
        if self.voltage_stress.is_empty() {
            return None;
        }
        Some(crate::types::VoltageStressResult {
            per_bus: self.voltage_stress,
            max_qv_stress_proxy: self.max_local_qv_stress_proxy,
            critical_proxy_bus: self.critical_proxy_bus,
            max_l_index: self.max_exact_l_index,
            critical_l_index_bus: self.critical_exact_l_bus,
            category: self.vsm_category,
        })
    }
}

fn merge_proxy_into_exact(
    proxy: VoltageStressSummary,
    mut exact: VoltageStressSummary,
) -> VoltageStressSummary {
    for (exact_bus, proxy_bus) in exact
        .voltage_stress
        .iter_mut()
        .zip(proxy.voltage_stress.iter())
    {
        if exact_bus.bus_number == proxy_bus.bus_number {
            exact_bus.local_qv_stress_proxy = proxy_bus.local_qv_stress_proxy;
        }
    }
    exact.max_local_qv_stress_proxy = proxy.max_local_qv_stress_proxy;
    exact.critical_proxy_bus = proxy.critical_proxy_bus;
    exact
}

/// Compute the cheap local Q-V stress proxy for each PQ bus in a converged
/// contingency result.
pub(crate) fn compute_voltage_stress_proxy(
    buses: &[Bus],
    ybus: &YBus,
    vm: &[f64],
    q_calc: &[f64],
) -> VoltageStressSummary {
    let mut voltage_stress = Vec::with_capacity(buses.len());
    let mut max_proxy = None;
    let mut critical_proxy_bus = None;

    for (k, bus) in buses.iter().enumerate() {
        let v = vm[k];
        let v_min = if bus.voltage_min_pu > 0.0 {
            bus.voltage_min_pu
        } else {
            0.95
        };
        let local_qv_stress_proxy = if bus.bus_type == BusType::PQ {
            let b_kk = ybus.b(k, k).abs();
            if b_kk > 1e-10 && v > 1e-6 {
                Some((q_calc[k].abs() / (v * v * b_kk)).clamp(0.0, 1.0))
            } else {
                Some(0.0)
            }
        } else {
            None
        };

        voltage_stress.push(BusVoltageStress {
            bus_number: bus.number,
            local_qv_stress_proxy,
            exact_l_index: None,
            voltage_margin_to_vmin: v - v_min,
        });

        if let Some(proxy) = local_qv_stress_proxy
            && max_proxy.is_none_or(|current| proxy > current)
        {
            max_proxy = Some(proxy);
            critical_proxy_bus = Some(bus.number);
        }
    }

    VoltageStressSummary {
        voltage_stress,
        max_local_qv_stress_proxy: max_proxy,
        critical_proxy_bus,
        ..Default::default()
    }
}

fn partition_exact_l_index_buses(network: &Network) -> (Vec<usize>, Vec<usize>) {
    let bus_map = network.bus_index_map();
    let mut live_gen_count = vec![0u32; network.buses.len()];
    for generator in &network.generators {
        if generator.in_service
            && let Some(&bus_idx) = bus_map.get(&generator.bus)
        {
            live_gen_count[bus_idx] += 1;
        }
    }

    let mut load_indices = Vec::new();
    let mut generator_indices = Vec::new();
    for (bus_idx, bus) in network.buses.iter().enumerate() {
        match bus.bus_type {
            BusType::PQ => load_indices.push(bus_idx),
            BusType::PV => {
                if live_gen_count[bus_idx] > 0 {
                    generator_indices.push(bus_idx);
                } else {
                    // Match the contingency solver's dead-PV demotion.
                    load_indices.push(bus_idx);
                }
            }
            BusType::Slack => generator_indices.push(bus_idx),
            BusType::Isolated => {}
        }
    }

    (load_indices, generator_indices)
}

fn complex_voltage(vm: &[f64], va: &[f64], bus_idx: usize) -> Complex64 {
    let magnitude = vm.get(bus_idx).copied().unwrap_or(1.0);
    let angle = va.get(bus_idx).copied().unwrap_or(0.0);
    Complex64::new(magnitude * angle.cos(), magnitude * angle.sin())
}

fn compute_f_lg(
    ybus: &YBus,
    load_indices: &[usize],
    generator_indices: &[usize],
) -> Option<Vec<Complex64>> {
    if load_indices.is_empty() || generator_indices.is_empty() {
        return Some(Vec::new());
    }

    let n_buses = ybus.n;
    let n_load = load_indices.len();
    let n_gen = generator_indices.len();

    let mut bus_to_load = vec![None; n_buses];
    for (load_local, &bus_idx) in load_indices.iter().enumerate() {
        bus_to_load[bus_idx] = Some(load_local);
    }

    let mut bus_to_gen = vec![None; n_buses];
    for (gen_local, &bus_idx) in generator_indices.iter().enumerate() {
        bus_to_gen[bus_idx] = Some(gen_local);
    }

    let mut y_ll_entries = Vec::new();
    let mut y_lg_columns: Vec<Vec<(usize, Complex64)>> = vec![Vec::new(); n_gen];

    for (load_local, &load_bus_idx) in load_indices.iter().enumerate() {
        let row = ybus.row(load_bus_idx);
        for (position, &col_bus_idx) in row.col_idx.iter().enumerate() {
            let value = Complex64::new(row.g[position], row.b[position]);
            if let Some(col_local) = bus_to_load[col_bus_idx] {
                y_ll_entries.push((load_local, col_local, value));
            } else if let Some(gen_local) = bus_to_gen[col_bus_idx] {
                y_lg_columns[gen_local].push((load_local, value));
            }
        }
    }

    y_ll_entries.sort_unstable_by_key(|&(row, col, _)| (col, row));

    let mut col_ptrs = vec![0usize; n_load + 1];
    let mut row_indices = Vec::with_capacity(y_ll_entries.len());
    let mut values = Vec::with_capacity(y_ll_entries.len());
    let mut entry_pos = 0usize;
    for col in 0..n_load {
        while entry_pos < y_ll_entries.len() && y_ll_entries[entry_pos].1 == col {
            row_indices.push(y_ll_entries[entry_pos].0);
            values.push(y_ll_entries[entry_pos].2);
            entry_pos += 1;
        }
        col_ptrs[col + 1] = row_indices.len();
    }

    let y_ll = CscMatrix::try_new(n_load, n_load, col_ptrs, row_indices, values).ok()?;
    let mut solver = ComplexKluSolver::new(&y_ll).ok()?;

    let mut f_lg = vec![Complex64::new(0.0, 0.0); n_load * n_gen];
    for (gen_local, column) in y_lg_columns.iter().enumerate() {
        let mut rhs = vec![Complex64::new(0.0, 0.0); n_load];
        for &(load_local, value) in column {
            rhs[load_local] = -value;
        }

        solver.solve_in_place(&mut rhs).ok()?;
        for (load_local, value) in rhs.into_iter().enumerate() {
            f_lg[load_local * n_gen + gen_local] = value;
        }
    }

    Some(f_lg)
}

pub(crate) fn compute_exact_voltage_stress(
    network: &Network,
    vm: &[f64],
    va: &[f64],
) -> VoltageStressSummary {
    let ybus = build_ybus(network);
    let (load_indices, generator_indices) = partition_exact_l_index_buses(network);
    let f_lg = compute_f_lg(&ybus, &load_indices, &generator_indices);

    let mut exact_l_index = vec![None; network.buses.len()];
    let mut max_exact = None;
    let mut critical_exact_bus = None;

    if let Some(f_lg) = f_lg
        && !generator_indices.is_empty()
    {
        for (load_local, &load_bus_idx) in load_indices.iter().enumerate() {
            let v_k = complex_voltage(vm, va, load_bus_idx);
            let l_index = if v_k.norm() > 1e-12 {
                let mut coupling_sum = Complex64::new(0.0, 0.0);
                for (gen_local, &gen_bus_idx) in generator_indices.iter().enumerate() {
                    let v_g = complex_voltage(vm, va, gen_bus_idx);
                    let f_kg = f_lg[load_local * generator_indices.len() + gen_local];
                    coupling_sum += f_kg * v_g / v_k;
                }
                Some((Complex64::new(1.0, 0.0) - coupling_sum).norm())
            } else {
                Some(0.0)
            };

            exact_l_index[load_bus_idx] = l_index;
            if let Some(value) = l_index
                && max_exact.is_none_or(|current| value > current)
            {
                max_exact = Some(value);
                critical_exact_bus = Some(network.buses[load_bus_idx].number);
            }
        }
    }

    let voltage_stress = network
        .buses
        .iter()
        .enumerate()
        .map(|(bus_idx, bus)| {
            let v_min = if bus.voltage_min_pu > 0.0 {
                bus.voltage_min_pu
            } else {
                0.95
            };
            BusVoltageStress {
                bus_number: bus.number,
                local_qv_stress_proxy: None,
                exact_l_index: exact_l_index[bus_idx],
                voltage_margin_to_vmin: vm.get(bus_idx).copied().unwrap_or(1.0) - v_min,
            }
        })
        .collect();

    VoltageStressSummary {
        voltage_stress,
        max_exact_l_index: max_exact,
        critical_exact_l_bus: critical_exact_bus,
        ..Default::default()
    }
}

pub(crate) fn compute_voltage_stress_summary(
    network: &Network,
    ybus: &YBus,
    vm: &[f64],
    va: &[f64],
    q_calc: &[f64],
    mode: &VoltageStressMode,
) -> VoltageStressSummary {
    match mode {
        VoltageStressMode::Off => VoltageStressSummary::default(),
        VoltageStressMode::Proxy => compute_voltage_stress_proxy(&network.buses, ybus, vm, q_calc),
        VoltageStressMode::ExactLIndex { l_index_threshold } => {
            let proxy = compute_voltage_stress_proxy(&network.buses, ybus, vm, q_calc);
            let mut summary =
                merge_proxy_into_exact(proxy, compute_exact_voltage_stress(network, vm, va));
            summary.vsm_category = summary
                .max_exact_l_index
                .map(|val| classify_l_index_category(val, *l_index_threshold));
            summary
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
    use surge_network::network::{Branch, Generator, Load};

    fn acpf_options() -> AcPfOptions {
        AcPfOptions {
            tolerance: 1e-8,
            max_iterations: 50,
            vm_min: 0.5,
            vm_max: 2.0,
            ..Default::default()
        }
    }

    fn build_three_bus_voltage_test_network() -> Network {
        let mut network = Network::new("exact_l_index_test");
        network.base_mva = 100.0;

        let mut bus1 = Bus::new(1, BusType::Slack, 138.0);
        bus1.voltage_magnitude_pu = 1.05;

        let bus2 = Bus::new(2, BusType::PQ, 138.0);

        let bus3 = Bus::new(3, BusType::PQ, 138.0);

        let mut generator = Generator::new(1, 220.0, 1.05);
        generator.pmax = 400.0;
        generator.qmax = 200.0;
        generator.qmin = -150.0;

        let mut br12 = Branch::new_line(1, 2, 0.02, 0.08, 0.02);
        br12.rating_a_mva = 300.0;
        let mut br13 = Branch::new_line(1, 3, 0.01, 0.05, 0.01);
        br13.phase_shift_rad = 5.0_f64.to_radians();
        br13.rating_a_mva = 250.0;
        let mut br23 = Branch::new_line(2, 3, 0.02, 0.10, 0.01);
        br23.rating_a_mva = 200.0;

        network.buses = vec![bus1, bus2, bus3];
        network.branches = vec![br12, br13, br23];
        network.generators = vec![generator];
        network.loads = vec![Load::new(2, 120.0, 40.0), Load::new(3, 55.0, 18.0)];
        network
    }

    fn solve_dense_complex(mut a: Vec<Vec<Complex64>>, mut b: Vec<Complex64>) -> Vec<Complex64> {
        let n = a.len();
        for col in 0..n {
            let mut pivot_row = col;
            let mut pivot_norm = a[col][col].norm();
            for (row, values) in a.iter().enumerate().skip(col + 1) {
                let norm = values[col].norm();
                if norm > pivot_norm {
                    pivot_row = row;
                    pivot_norm = norm;
                }
            }
            assert!(
                pivot_norm > 1e-12,
                "dense complex system should be nonsingular"
            );
            if pivot_row != col {
                a.swap(col, pivot_row);
                b.swap(col, pivot_row);
            }

            let pivot = a[col][col];
            let pivot_tail: Vec<Complex64> = a[col][col..].to_vec();
            let b_col = b[col];
            for row in (col + 1)..n {
                let factor = a[row][col] / pivot;
                if factor.norm() < 1e-15 {
                    continue;
                }
                for (offset, pivot_entry) in pivot_tail.iter().enumerate() {
                    a[row][col + offset] -= factor * *pivot_entry;
                }
                b[row] -= factor * b_col;
            }
        }

        let mut x = vec![Complex64::new(0.0, 0.0); n];
        for row in (0..n).rev() {
            let mut sum = b[row];
            for (col, value) in a[row].iter().enumerate().skip(row + 1) {
                sum -= *value * x[col];
            }
            x[row] = sum / a[row][row];
        }
        x
    }

    fn manual_exact_l_indices(network: &Network, vm: &[f64], va: &[f64]) -> Vec<(u32, f64)> {
        let ybus = build_ybus(network);
        let (load_indices, generator_indices) = partition_exact_l_index_buses(network);
        let mut y_ll = vec![vec![Complex64::new(0.0, 0.0); load_indices.len()]; load_indices.len()];
        let mut y_lg =
            vec![vec![Complex64::new(0.0, 0.0); generator_indices.len()]; load_indices.len()];

        for (load_row, &load_bus_idx) in load_indices.iter().enumerate() {
            for (load_col, &load_bus_col_idx) in load_indices.iter().enumerate() {
                y_ll[load_row][load_col] = ybus.at(load_bus_idx, load_bus_col_idx);
            }
            for (gen_col, &gen_bus_idx) in generator_indices.iter().enumerate() {
                y_lg[load_row][gen_col] = ybus.at(load_bus_idx, gen_bus_idx);
            }
        }

        let voltages: Vec<Complex64> = (0..network.buses.len())
            .map(|bus_idx| complex_voltage(vm, va, bus_idx))
            .collect();

        let mut result = Vec::with_capacity(load_indices.len());
        for (load_row, &load_bus_idx) in load_indices.iter().enumerate() {
            let rhs: Vec<Complex64> = (0..load_indices.len()).map(|i| -y_lg[i][0]).collect();
            let _ = rhs;
            let mut sum = Complex64::new(0.0, 0.0);
            for gen_col in 0..generator_indices.len() {
                let rhs: Vec<Complex64> =
                    (0..load_indices.len()).map(|i| -y_lg[i][gen_col]).collect();
                let f_col = solve_dense_complex(y_ll.clone(), rhs);
                sum +=
                    f_col[load_row] * voltages[generator_indices[gen_col]] / voltages[load_bus_idx];
            }
            result.push((
                network.buses[load_bus_idx].number,
                (Complex64::new(1.0, 0.0) - sum).norm(),
            ));
        }
        result
    }

    #[test]
    fn exact_l_index_is_finite_on_asymmetric_network() {
        let network = build_three_bus_voltage_test_network();
        let solution = solve_ac_pf_kernel(&network, &acpf_options()).expect("NR should converge");
        let stress = compute_exact_voltage_stress(
            &network,
            &solution.voltage_magnitude_pu,
            &solution.voltage_angle_rad,
        );

        assert_eq!(stress.voltage_stress.len(), 3);
        let bus2 = stress
            .voltage_stress
            .iter()
            .find(|entry| entry.bus_number == 2)
            .expect("bus 2 stress entry");
        let bus3 = stress
            .voltage_stress
            .iter()
            .find(|entry| entry.bus_number == 3)
            .expect("bus 3 stress entry");

        let l2 = bus2.exact_l_index.expect("bus 2 exact L-index");
        let l3 = bus3.exact_l_index.expect("bus 3 exact L-index");
        assert!(l2.is_finite() && (0.0..1.0).contains(&l2));
        assert!(l3.is_finite() && (0.0..1.0).contains(&l3));
        assert_eq!(
            stress
                .voltage_stress
                .iter()
                .find(|entry| entry.bus_number == 1)
                .expect("slack stress entry")
                .exact_l_index,
            None
        );
        assert_eq!(stress.max_exact_l_index, Some(l2.max(l3)));
    }

    #[test]
    fn exact_l_index_treats_dead_pv_bus_as_load_bus() {
        let mut network = Network::new("dead_pv_exact_l_index");
        network.base_mva = 100.0;

        let mut bus1 = Bus::new(1, BusType::Slack, 138.0);
        bus1.voltage_magnitude_pu = 1.04;
        let bus2 = Bus::new(2, BusType::PV, 138.0);
        let bus3 = Bus::new(3, BusType::PQ, 138.0);

        let mut slack = Generator::new(1, 120.0, 1.04);
        slack.pmax = 200.0;
        let mut dead_pv_generator = Generator::new(2, 35.0, 1.01);
        dead_pv_generator.in_service = false;

        network.buses = vec![bus1, bus2, bus3];
        network.loads = vec![Load::new(2, 40.0, 10.0), Load::new(3, 25.0, 8.0)];
        network.branches = vec![
            Branch::new_line(1, 2, 0.02, 0.08, 0.02),
            Branch::new_line(2, 3, 0.02, 0.09, 0.02),
            Branch::new_line(1, 3, 0.03, 0.12, 0.01),
        ];
        network.generators = vec![slack, dead_pv_generator];

        let stress =
            compute_exact_voltage_stress(&network, &[1.04, 0.99, 0.97], &[0.0, -0.03, -0.05]);
        let dead_pv_entry = stress
            .voltage_stress
            .iter()
            .find(|entry| entry.bus_number == 2)
            .expect("dead PV bus stress entry");

        assert!(
            dead_pv_entry.exact_l_index.is_some(),
            "dead PV buses should be demoted to the load partition for exact L-index"
        );
    }

    #[test]
    fn exact_l_index_matches_manual_dense_reference() {
        let network = build_three_bus_voltage_test_network();
        let solution = solve_ac_pf_kernel(&network, &acpf_options()).expect("NR should converge");
        let stress = compute_exact_voltage_stress(
            &network,
            &solution.voltage_magnitude_pu,
            &solution.voltage_angle_rad,
        );
        let manual = manual_exact_l_indices(
            &network,
            &solution.voltage_magnitude_pu,
            &solution.voltage_angle_rad,
        );

        for (bus_number, manual_value) in manual {
            let computed_value = stress
                .voltage_stress
                .iter()
                .find(|entry| entry.bus_number == bus_number)
                .and_then(|entry| entry.exact_l_index)
                .expect("computed exact L-index");
            assert!(
                (computed_value - manual_value).abs() < 1e-10,
                "bus {bus_number}: sparse exact L-index {computed_value:.12} disagrees with dense reference {manual_value:.12}"
            );
        }
    }

    #[test]
    fn exact_l_index_increases_when_network_is_stressed() {
        let base = build_three_bus_voltage_test_network();
        let mut stressed = base.clone();
        // Scale loads: load[0] is bus 2, load[1] is bus 3
        stressed.loads[0].active_power_demand_mw *= 1.6;
        stressed.loads[0].reactive_power_demand_mvar *= 1.6;
        stressed.loads[1].active_power_demand_mw *= 1.4;
        stressed.loads[1].reactive_power_demand_mvar *= 1.4;

        let base_solution =
            solve_ac_pf_kernel(&base, &acpf_options()).expect("base NR should converge");
        let stressed_solution =
            solve_ac_pf_kernel(&stressed, &acpf_options()).expect("stressed NR should converge");

        let base_stress = compute_exact_voltage_stress(
            &base,
            &base_solution.voltage_magnitude_pu,
            &base_solution.voltage_angle_rad,
        );
        let stressed_stress = compute_exact_voltage_stress(
            &stressed,
            &stressed_solution.voltage_magnitude_pu,
            &stressed_solution.voltage_angle_rad,
        );

        let base_max = base_stress
            .max_exact_l_index
            .expect("base exact L-index maximum");
        let stressed_max = stressed_stress
            .max_exact_l_index
            .expect("stressed exact L-index maximum");
        assert!(
            stressed_max > base_max,
            "stressed case max exact L-index ({stressed_max:.6}) should exceed base case ({base_max:.6})"
        );
    }
}
