// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Jacobian matrix assembly for Newton-Raphson power flow.
//!
//! The Jacobian J = [H N; M L] maps voltage corrections to power mismatches:
//!   \[ΔP\]   \[H  N\] \[Δθ \]
//!   \[ΔQ\] = \[M  L\] \[ΔVm\]
//!
//! H = dP/dθ,  N = dP/dVm,  M = dQ/dθ,  L = dQ/dVm
//!
//! The sparsity pattern is pre-computed once from the Y-bus structure.
//! Each NR iteration only fills numerical values using the cached pattern,
//! eliminating triplet allocation, sorting, and CSC construction overhead.

use faer::sparse::{Argsort, Pair, SparseColMat, SymbolicSparseColMat};

use crate::matrix::SENTINEL;
use crate::matrix::ybus::YBus;

/// Describes one non-zero entry in the Jacobian and how to compute its value.
#[derive(Clone, Copy)]
enum JacEntry {
    /// H_ii = -Q_i - B_ii * Vm_i²
    HDiag { i: usize, b_ii: f64 },
    /// H_ij = Vm_i * Vm_j * (G_ij * sin(θ_ij) - B_ij * cos(θ_ij))
    HOff {
        i: usize,
        j: usize,
        g_ij: f64,
        b_ij: f64,
    },
    /// N_ii = P_i / Vm_i + G_ii * Vm_i
    NDiag { i: usize, g_ii: f64 },
    /// N_ij = Vm_i * (G_ij * cos(θ_ij) + B_ij * sin(θ_ij))
    NOff {
        i: usize,
        j: usize,
        g_ij: f64,
        b_ij: f64,
    },
    /// M_ii = P_i - G_ii * Vm_i²
    MDiag { i: usize, g_ii: f64 },
    /// M_ij = -Vm_i * Vm_j * (G_ij * cos(θ_ij) + B_ij * sin(θ_ij))
    MOff {
        i: usize,
        j: usize,
        g_ij: f64,
        b_ij: f64,
    },
    /// L_ii = Q_i / Vm_i - B_ii * Vm_i
    LDiag { i: usize, b_ii: f64 },
    /// L_ij = Vm_i * (G_ij * sin(θ_ij) - B_ij * cos(θ_ij))
    LOff {
        i: usize,
        j: usize,
        g_ij: f64,
        b_ij: f64,
    },
}

/// Pre-computed Jacobian sparsity pattern with cached Y-bus admittance values.
///
/// Built once from the Y-bus and bus classification, then reused across all
/// NR iterations. Each iteration only fills numerical values using `build()`.
pub struct JacobianPattern {
    dim: usize,
    entries: Vec<JacEntry>,
    symbolic: SymbolicSparseColMat<usize>,
    argsort: Argsort<usize>,
}

impl JacobianPattern {
    /// Pre-compute the Jacobian sparsity pattern from Y-bus structure.
    pub fn new(ybus: &YBus, pvpq: &[usize], pq: &[usize]) -> Self {
        let n_pvpq = pvpq.len();
        let n_pq = pq.len();
        let dim = n_pvpq + n_pq;

        // Reverse lookup maps
        let mut pvpq_pos = vec![SENTINEL; ybus.n];
        for (pos, &bus) in pvpq.iter().enumerate() {
            pvpq_pos[bus] = pos;
        }
        let mut pq_pos = vec![SENTINEL; ybus.n];
        for (pos, &bus) in pq.iter().enumerate() {
            pq_pos[bus] = pos;
        }

        let mut indices: Vec<Pair<usize, usize>> = Vec::with_capacity(4 * ybus.nnz);
        let mut entries: Vec<JacEntry> = Vec::with_capacity(4 * ybus.nnz);

        // H and N sub-matrices (rows from pvpq buses)
        for (row_h, &i) in pvpq.iter().enumerate() {
            let row_ybus = ybus.row(i);

            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];

                if i == j {
                    // H_ii
                    indices.push(Pair::new(row_h, row_h));
                    entries.push(JacEntry::HDiag { i, b_ii: b_ij });

                    // N_ii (only PQ buses)
                    if pq_pos[i] != SENTINEL {
                        indices.push(Pair::new(row_h, n_pvpq + pq_pos[i]));
                        entries.push(JacEntry::NDiag { i, g_ii: g_ij });
                    }
                } else {
                    // H_ij
                    if pvpq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_h, pvpq_pos[j]));
                        entries.push(JacEntry::HOff { i, j, g_ij, b_ij });
                    }

                    // N_ij
                    if pq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_h, n_pvpq + pq_pos[j]));
                        entries.push(JacEntry::NOff { i, j, g_ij, b_ij });
                    }
                }
            }
        }

        // M and L sub-matrices (rows from pq buses)
        for (row_offset, &i) in pq.iter().enumerate() {
            let row_m = n_pvpq + row_offset;
            let row_ybus = ybus.row(i);

            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];

                if i == j {
                    // M_ii
                    indices.push(Pair::new(row_m, pvpq_pos[i]));
                    entries.push(JacEntry::MDiag { i, g_ii: g_ij });

                    // L_ii
                    indices.push(Pair::new(row_m, n_pvpq + pq_pos[i]));
                    entries.push(JacEntry::LDiag { i, b_ii: b_ij });
                } else {
                    // M_ij
                    if pvpq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_m, pvpq_pos[j]));
                        entries.push(JacEntry::MOff { i, j, g_ij, b_ij });
                    }

                    // L_ij
                    if pq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_m, n_pvpq + pq_pos[j]));
                        entries.push(JacEntry::LOff { i, j, g_ij, b_ij });
                    }
                }
            }
        }

        let (symbolic, argsort) =
            SymbolicSparseColMat::<usize>::try_new_from_indices(dim, dim, &indices)
                .expect("Jacobian symbolic pattern construction failed");

        Self {
            dim,
            entries,
            symbolic,
            argsort,
        }
    }

    /// Get the symbolic sparsity pattern (for SymbolicLu construction).
    pub fn symbolic(&self) -> &SymbolicSparseColMat<usize> {
        &self.symbolic
    }

    /// Dimension of the Jacobian matrix.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Fill Jacobian values from current voltage state and power injections.
    ///
    /// Returns a sparse CSC matrix using the pre-computed pattern.
    pub fn build(
        &self,
        vm: &[f64],
        va: &[f64],
        p_calc: &[f64],
        q_calc: &[f64],
    ) -> SparseColMat<usize, f64> {
        let mut values = Vec::with_capacity(self.entries.len());

        for entry in &self.entries {
            let val = match *entry {
                JacEntry::HDiag { i, b_ii } => -q_calc[i] - b_ii * vm[i] * vm[i],
                JacEntry::HOff { i, j, g_ij, b_ij } => {
                    let theta_ij = va[i] - va[j];
                    vm[i] * vm[j] * (g_ij * theta_ij.sin() - b_ij * theta_ij.cos())
                }
                JacEntry::NDiag { i, g_ii } => {
                    let vm_safe = vm[i].max(1e-6);
                    p_calc[i] / vm_safe + g_ii * vm[i]
                }
                JacEntry::NOff { i, j, g_ij, b_ij } => {
                    let theta_ij = va[i] - va[j];
                    vm[i] * (g_ij * theta_ij.cos() + b_ij * theta_ij.sin())
                }
                JacEntry::MDiag { i, g_ii } => p_calc[i] - g_ii * vm[i] * vm[i],
                JacEntry::MOff { i, j, g_ij, b_ij } => {
                    let theta_ij = va[i] - va[j];
                    -vm[i] * vm[j] * (g_ij * theta_ij.cos() + b_ij * theta_ij.sin())
                }
                JacEntry::LDiag { i, b_ii } => {
                    let vm_safe = vm[i].max(1e-6);
                    q_calc[i] / vm_safe - b_ii * vm[i]
                }
                JacEntry::LOff { i, j, g_ij, b_ij } => {
                    let theta_ij = va[i] - va[j];
                    vm[i] * (g_ij * theta_ij.sin() - b_ij * theta_ij.cos())
                }
            };
            values.push(val);
        }

        SparseColMat::new_from_argsort(self.symbolic.clone(), &self.argsort, &values)
            .expect("Jacobian value fill failed")
    }
}

/// Build the sparse Jacobian (one-shot, no pattern reuse).
///
/// Kept for backward compatibility and testing. For NR iteration, use
/// `JacobianPattern::new()` + `pattern.build()` instead.
pub fn build_jacobian(
    ybus: &YBus,
    vm: &[f64],
    va: &[f64],
    p_calc: &[f64],
    q_calc: &[f64],
    pvpq: &[usize],
    pq: &[usize],
) -> SparseColMat<usize, f64> {
    let pattern = JacobianPattern::new(ybus, pvpq, pq);
    pattern.build(vm, va, p_calc, q_calc)
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn data_available() -> bool {
        crate::test_cases::case_available("case9")
    }
    #[allow(dead_code)]
    fn test_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::new()
    }

    use super::*;
    use crate::matrix::mismatch::compute_power_injection;
    use crate::matrix::ybus::build_ybus;
    use crate::solver::newton_raphson::{AcPfOptions, solve_ac_pf};

    use surge_network::Network;
    use surge_network::network::BusType;

    fn load_case(name: &str) -> Network {
        crate::test_cases::load_case(name)
            .unwrap_or_else(|err| panic!("failed to load {name} fixture: {err}"))
    }

    /// Classify buses into (pvpq sorted, pq) index vectors, matching NR solver logic.
    fn classify_buses(net: &Network) -> (Vec<usize>, Vec<usize>) {
        let mut pv_idx: Vec<usize> = Vec::new();
        let mut pq_idx: Vec<usize> = Vec::new();
        for (i, bus) in net.buses.iter().enumerate() {
            match bus.bus_type {
                BusType::PV => pv_idx.push(i),
                BusType::PQ => pq_idx.push(i),
                _ => {} // Slack excluded from Jacobian
            }
        }
        let mut pvpq: Vec<usize> = Vec::with_capacity(pv_idx.len() + pq_idx.len());
        pvpq.extend(&pv_idx);
        pvpq.extend(&pq_idx);
        pvpq.sort_unstable();
        (pvpq, pq_idx)
    }

    /// Convert a sparse CSC Jacobian to a dense 2D array for element-by-element access.
    ///
    /// The Jacobian is dim x dim. Sparse entries are read from the CSC structure;
    /// all other entries default to 0.0.
    fn sparse_to_dense(jac: &SparseColMat<usize, f64>, dim: usize) -> Vec<Vec<f64>> {
        let mut dense = vec![vec![0.0; dim]; dim];
        let jac_ref = jac.as_ref();
        let symbolic = jac_ref.symbolic();
        let col_ptrs: Vec<usize> = symbolic.col_ptr().to_vec();
        let row_indices: Vec<usize> = symbolic.row_idx().to_vec();
        let values = jac_ref.val();

        for col in 0..dim {
            for idx in col_ptrs[col]..col_ptrs[col + 1] {
                let row = row_indices[idx];
                dense[row][col] = values[idx];
            }
        }
        dense
    }

    /// Compute the numerical Jacobian via central finite differences.
    ///
    /// For each variable x_j (theta for pvpq, Vm for pq), perturbs by +/- eps
    /// and computes: J_FD[i,j] = (f(x+eps*e_j) - f(x-eps*e_j)) / (2*eps)
    /// where f = [P_calc(pvpq); Q_calc(pq)].
    fn compute_fd_jacobian(
        ybus: &YBus,
        vm: &[f64],
        va: &[f64],
        pvpq: &[usize],
        pq: &[usize],
        eps: f64,
    ) -> Vec<Vec<f64>> {
        let n_pvpq = pvpq.len();
        let n_pq = pq.len();
        let dim = n_pvpq + n_pq;
        let mut jac_fd = vec![vec![0.0; dim]; dim];

        for col in 0..dim {
            let mut vm_plus = vm.to_vec();
            let mut va_plus = va.to_vec();
            let mut vm_minus = vm.to_vec();
            let mut va_minus = va.to_vec();

            if col < n_pvpq {
                // Perturbing theta[pvpq[col]]
                let bus = pvpq[col];
                va_plus[bus] += eps;
                va_minus[bus] -= eps;
            } else {
                // Perturbing Vm[pq[col - n_pvpq]]
                let bus = pq[col - n_pvpq];
                vm_plus[bus] += eps;
                vm_minus[bus] -= eps;
            }

            let (p_plus, q_plus) = compute_power_injection(ybus, &vm_plus, &va_plus);
            let (p_minus, q_minus) = compute_power_injection(ybus, &vm_minus, &va_minus);

            for row in 0..dim {
                jac_fd[row][col] = if row < n_pvpq {
                    // Row = P_calc[pvpq[row]]
                    let bus = pvpq[row];
                    (p_plus[bus] - p_minus[bus]) / (2.0 * eps)
                } else {
                    // Row = Q_calc[pq[row - n_pvpq]]
                    let bus = pq[row - n_pvpq];
                    (q_plus[bus] - q_minus[bus]) / (2.0 * eps)
                };
            }
        }

        jac_fd
    }

    /// Label a Jacobian entry for diagnostic messages.
    fn label_entry(row: usize, col: usize, n_pvpq: usize, pvpq: &[usize], pq: &[usize]) -> String {
        let row_label = if row < n_pvpq {
            format!("P[bus{}]", pvpq[row])
        } else {
            format!("Q[bus{}]", pq[row - n_pvpq])
        };
        let col_label = if col < n_pvpq {
            format!("theta[bus{}]", pvpq[col])
        } else {
            format!("Vm[bus{}]", pq[col - n_pvpq])
        };
        let block = match (row < n_pvpq, col < n_pvpq) {
            (true, true) => "H(dP/dtheta)",
            (true, false) => "N(dP/dVm)",
            (false, true) => "M(dQ/dtheta)",
            (false, false) => "L(dQ/dVm)",
        };
        format!("{row_label} x {col_label} [{block}]")
    }

    /// Validate analytical Jacobian against numerical finite-difference approximation.
    ///
    /// Uses central differences with eps=1e-6 on case9 at the converged operating point.
    /// This catches Jacobian assembly bugs that could cause NR to converge to wrong
    /// solutions or diverge.
    ///
    /// The Jacobian J = [H N; M L] maps (delta_theta, delta_Vm) -> (delta_P, delta_Q):
    ///   - Rows: P_calc for PVPQ buses, then Q_calc for PQ buses
    ///   - Cols: theta for PVPQ buses, then Vm for PQ buses
    ///
    /// For each column j, we perturb the corresponding variable by +/- eps and compute:
    ///   J_FD[:, j] = (f(x + eps*e_j) - f(x - eps*e_j)) / (2*eps)
    /// where f = [P_calc(pvpq); Q_calc(pq)].
    #[test]
    fn test_jacobian_finite_difference_validation() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let opts = AcPfOptions {
            enforce_q_limits: false,
            detect_islands: false,
            ..AcPfOptions::default()
        };
        let sol = solve_ac_pf(&net, &opts).expect("NR should converge on case9");

        let ybus = build_ybus(&net);
        let vm = &sol.voltage_magnitude_pu;
        let va = &sol.voltage_angle_rad;

        let (pvpq, pq) = classify_buses(&net);
        let n_pvpq = pvpq.len();
        let dim = n_pvpq + pq.len();

        // Compute power injections at the converged point
        let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);

        // Build analytical Jacobian
        let jac = build_jacobian(&ybus, vm, va, &p_calc, &q_calc, &pvpq, &pq);
        let jac_dense = sparse_to_dense(&jac, dim);

        // Build numerical Jacobian via central finite differences
        let eps = 1e-6;
        let jac_fd = compute_fd_jacobian(&ybus, vm, va, &pvpq, &pq, eps);

        // Compare element-by-element
        let tol = 1e-4;
        let mut max_err = 0.0_f64;
        let mut max_err_loc = (0, 0);

        for row in 0..dim {
            for col in 0..dim {
                let analytical = jac_dense[row][col];
                let numerical = jac_fd[row][col];
                let err = (analytical - numerical).abs();
                if err > max_err {
                    max_err = err;
                    max_err_loc = (row, col);
                }
                assert!(
                    err < tol,
                    "case9 Jacobian mismatch at ({row},{col}): analytical={analytical:.8},                      FD={numerical:.8}, err={err:.2e}
  {}",
                    label_entry(row, col, n_pvpq, &pvpq, &pq)
                );
            }
        }

        eprintln!(
            "case9 Jacobian FD validation PASSED: dim={dim}, max_err={max_err:.2e} at ({},{})",
            max_err_loc.0, max_err_loc.1
        );
    }

    /// Validate analytical Jacobian against FD on case14 (includes transformers
    /// with off-nominal taps and bus shunts -- exercises more Y-bus code paths).
    #[test]
    fn test_jacobian_finite_difference_case14() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let opts = AcPfOptions {
            enforce_q_limits: false,
            detect_islands: false,
            ..AcPfOptions::default()
        };
        let sol = solve_ac_pf(&net, &opts).expect("NR should converge on case14");

        let ybus = build_ybus(&net);
        let vm = &sol.voltage_magnitude_pu;
        let va = &sol.voltage_angle_rad;

        let (pvpq, pq) = classify_buses(&net);
        let n_pvpq = pvpq.len();
        let dim = n_pvpq + pq.len();

        let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);
        let jac = build_jacobian(&ybus, vm, va, &p_calc, &q_calc, &pvpq, &pq);
        let jac_dense = sparse_to_dense(&jac, dim);

        let eps = 1e-6;
        let jac_fd = compute_fd_jacobian(&ybus, vm, va, &pvpq, &pq, eps);

        let tol = 1e-4;
        let mut max_err = 0.0_f64;

        for row in 0..dim {
            for col in 0..dim {
                let analytical = jac_dense[row][col];
                let numerical = jac_fd[row][col];
                let err = (analytical - numerical).abs();
                max_err = max_err.max(err);
                assert!(
                    err < tol,
                    "case14 Jacobian mismatch at ({row},{col}): analytical={analytical:.8},                      FD={numerical:.8}, err={err:.2e}
  {}",
                    label_entry(row, col, n_pvpq, &pvpq, &pq)
                );
            }
        }

        eprintln!("case14 Jacobian FD validation PASSED: dim={dim}, max_err={max_err:.2e}");
    }

    /// Validate Jacobian FD on case118 (larger network, diverse topology).
    ///
    /// Uses combined absolute + relative tolerance since case118 has larger
    /// Jacobian entries where absolute FD error scales with the entry magnitude.
    #[test]
    fn test_jacobian_finite_difference_case118() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case118");
        let opts = AcPfOptions {
            enforce_q_limits: false,
            detect_islands: false,
            ..AcPfOptions::default()
        };
        let sol = solve_ac_pf(&net, &opts).expect("NR should converge on case118");

        let ybus = build_ybus(&net);
        let vm = &sol.voltage_magnitude_pu;
        let va = &sol.voltage_angle_rad;

        let (pvpq, pq) = classify_buses(&net);
        let n_pvpq = pvpq.len();
        let dim = n_pvpq + pq.len();

        let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);
        let jac = build_jacobian(&ybus, vm, va, &p_calc, &q_calc, &pvpq, &pq);
        let jac_dense = sparse_to_dense(&jac, dim);

        let eps = 1e-6;
        let jac_fd = compute_fd_jacobian(&ybus, vm, va, &pvpq, &pq, eps);

        let abs_tol: f64 = 1e-4;
        let rel_tol: f64 = 1e-6;
        let mut max_err = 0.0_f64;
        let mut n_checked = 0usize;

        for row in 0..dim {
            for col in 0..dim {
                let analytical = jac_dense[row][col];
                let numerical = jac_fd[row][col];
                let err = (analytical - numerical).abs();
                let threshold = abs_tol.max(rel_tol * analytical.abs());
                max_err = max_err.max(err);
                n_checked += 1;

                assert!(
                    err < threshold,
                    "case118 Jacobian mismatch at ({row},{col}): analytical={analytical:.8},                      FD={numerical:.8}, err={err:.2e}, threshold={threshold:.2e}
  {}",
                    label_entry(row, col, n_pvpq, &pvpq, &pq)
                );
            }
        }

        eprintln!(
            "case118 Jacobian FD validation PASSED: dim={dim}, {n_checked} elements, max_err={max_err:.2e}"
        );
    }

    /// Validate FusedPattern Jacobian matches the standalone build_jacobian.
    ///
    /// Both paths should produce identical Jacobian values. This ensures the fused
    /// (single-pass mismatch + Jacobian) implementation matches the reference.
    #[test]
    fn test_fused_jacobian_matches_standalone() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        use crate::matrix::fused::FusedPattern;

        for case_name in &["case9", "case14", "case118"] {
            let net = load_case(case_name);
            let opts = AcPfOptions {
                enforce_q_limits: false,
                detect_islands: false,
                ..AcPfOptions::default()
            };
            let sol = solve_ac_pf(&net, &opts)
                .unwrap_or_else(|e| panic!("{case_name} NR should converge: {e}"));

            let ybus = build_ybus(&net);
            let vm = &sol.voltage_magnitude_pu;
            let va = &sol.voltage_angle_rad;

            let (pvpq, pq) = classify_buses(&net);
            let n_pvpq = pvpq.len();
            let dim = n_pvpq + pq.len();

            // Standalone Jacobian (reference)
            let (p_calc, q_calc) = compute_power_injection(&ybus, vm, va);
            let jac_standalone = build_jacobian(&ybus, vm, va, &p_calc, &q_calc, &pvpq, &pq);
            let dense_standalone = sparse_to_dense(&jac_standalone, dim);

            // Fused Jacobian
            let fused_pattern = FusedPattern::new(&ybus, &pvpq, &pq);
            let (p_fused, q_fused, jac_fused) = fused_pattern.build_fused(&ybus, vm, va);
            let dense_fused = sparse_to_dense(&jac_fused, dim);

            // Power injections should match exactly
            for i in 0..net.n_buses() {
                assert!(
                    (p_calc[i] - p_fused[i]).abs() < 1e-12,
                    "{case_name} P mismatch at bus {i}: standalone={}, fused={}",
                    p_calc[i],
                    p_fused[i]
                );
                assert!(
                    (q_calc[i] - q_fused[i]).abs() < 1e-12,
                    "{case_name} Q mismatch at bus {i}: standalone={}, fused={}",
                    q_calc[i],
                    q_fused[i]
                );
            }

            // Jacobian values should match within machine epsilon
            let tol = 1e-10;
            let mut max_err = 0.0_f64;
            for row in 0..dim {
                for col in 0..dim {
                    let err = (dense_standalone[row][col] - dense_fused[row][col]).abs();
                    max_err = max_err.max(err);
                    assert!(
                        err < tol,
                        "{case_name} fused vs standalone mismatch at ({row},{col}):                          standalone={:.12}, fused={:.12}",
                        dense_standalone[row][col],
                        dense_fused[row][col]
                    );
                }
            }

            eprintln!("{case_name} fused vs standalone PASSED: dim={dim}, max_err={max_err:.2e}");
        }
    }

    /// Validate Jacobian FD at a non-converged (flat-start) operating point.
    ///
    /// The FD validation should hold at ANY operating point, not just the
    /// converged solution. Testing at flat start (Vm=1, Va=0) catches bugs
    /// that might be masked at the solution point where mismatches are near zero.
    #[test]
    fn test_jacobian_finite_difference_flat_start() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let ybus = build_ybus(&net);
        let n = net.n_buses();

        // Flat start: Vm = 1.0, Va = 0.0 (except use generator Vm setpoints)
        let mut vm = vec![1.0; n];
        let va = vec![0.0; n];
        let bus_map = net.bus_index_map();
        for g in &net.generators {
            if g.in_service
                && let Some(&idx) = bus_map.get(&g.bus)
                && (net.buses[idx].bus_type == BusType::PV
                    || net.buses[idx].bus_type == BusType::Slack)
            {
                vm[idx] = g.voltage_setpoint_pu;
            }
        }

        let (pvpq, pq) = classify_buses(&net);
        let n_pvpq = pvpq.len();
        let dim = n_pvpq + pq.len();

        let (p_calc, q_calc) = compute_power_injection(&ybus, &vm, &va);
        let jac = build_jacobian(&ybus, &vm, &va, &p_calc, &q_calc, &pvpq, &pq);
        let jac_dense = sparse_to_dense(&jac, dim);

        let eps = 1e-6;
        let jac_fd = compute_fd_jacobian(&ybus, &vm, &va, &pvpq, &pq, eps);

        let tol = 1e-4;
        let mut max_err = 0.0_f64;

        for row in 0..dim {
            for col in 0..dim {
                let analytical = jac_dense[row][col];
                let numerical = jac_fd[row][col];
                let err = (analytical - numerical).abs();
                max_err = max_err.max(err);
                assert!(
                    err < tol,
                    "case9 flat-start Jacobian mismatch at ({row},{col}): analytical={analytical:.8},                      FD={numerical:.8}, err={err:.2e}
  {}",
                    label_entry(row, col, n_pvpq, &pvpq, &pq)
                );
            }
        }

        eprintln!(
            "case9 flat-start Jacobian FD validation PASSED: dim={dim}, max_err={max_err:.2e}"
        );
    }
}
