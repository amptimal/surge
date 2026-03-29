// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Fused mismatch + Jacobian computation for Newton-Raphson power flow.
//!
//! Performs a single pass over the Y-bus to compute both power injections (P, Q)
//! and Jacobian values simultaneously. This halves the number of sin/cos calls
//! per NR iteration compared to separate mismatch + Jacobian passes.
//!
//! The sparsity pattern is pre-computed once and reused across iterations,
//! just like `JacobianPattern`. The key difference is that `build_fused()`
//! returns (p_calc, q_calc, Jacobian) in a single Y-bus traversal.

use faer::sparse::{Pair, SparseColMat, SymbolicSparseColMat};

use crate::matrix::SENTINEL;
use crate::matrix::ybus::YBus;

/// Off-diagonal Y-bus entry (i,j) with pre-computed Jacobian value slot indices.
///
/// Stores a CSR position (`ybus_pos`) instead of cached G/B values, so the
/// pattern can be reused across contingencies with different Y-bus values.
#[derive(Clone, Copy)]
struct FusedOffDiag {
    j: usize,
    /// Position in Y-bus CSR arrays (g_vals[ybus_pos], b_vals[ybus_pos]).
    ybus_pos: usize,
    /// Index into values array for H_ij, or SENTINEL if not present.
    h_idx: usize,
    /// Index into values array for N_ij, or SENTINEL if not present.
    n_idx: usize,
    /// Index into values array for M_ij, or SENTINEL if not present.
    m_idx: usize,
    /// Index into values array for L_ij, or SENTINEL if not present.
    l_idx: usize,
}

/// Diagonal Y-bus entry for bus i with pre-computed Jacobian value slot indices.
///
/// Stores a CSR position (`ybus_pos`) instead of cached G/B values.
#[derive(Clone, Copy)]
struct FusedDiag {
    /// Position in Y-bus CSR arrays (g_vals[ybus_pos], b_vals[ybus_pos]).
    ybus_pos: usize,
    /// Index into values array for H_ii, or SENTINEL.
    h_idx: usize,
    /// Index into values array for N_ii, or SENTINEL.
    n_idx: usize,
    /// Index into values array for M_ii, or SENTINEL.
    m_idx: usize,
    /// Index into values array for L_ii, or SENTINEL.
    l_idx: usize,
}

/// Pre-computed pattern for fused mismatch + Jacobian computation.
///
/// Built once from the Y-bus and bus classification, then reused across all
/// NR iterations. Each iteration calls `build_fused()` which does a single
/// Y-bus pass computing both power injections and Jacobian values.
///
/// The fused entries store CSC-order indices directly, so values are written
/// in column-compressed order without a permutation step. This enables
/// zero-allocation iteration: just fill a pre-allocated values buffer and
/// wrap it as a `SparseColMatRef`.
pub struct FusedPattern {
    n: usize,
    dim: usize,
    /// bus_ptr[i]..bus_ptr[i+1] indexes into off_diag for bus i's off-diagonal entries.
    bus_ptr: Vec<usize>,
    off_diag: Vec<FusedOffDiag>,
    diag: Vec<FusedDiag>,
    /// Number of nonzeros in the CSC Jacobian.
    nnz_csc: usize,
    symbolic: SymbolicSparseColMat<usize>,
}

impl FusedPattern {
    #[inline]
    fn fill_angle_trig(&self, va: &[f64], sin_va: &mut [f64], cos_va: &mut [f64]) {
        debug_assert!(sin_va.len() >= self.n);
        debug_assert!(cos_va.len() >= self.n);
        for i in 0..self.n {
            (sin_va[i], cos_va[i]) = va[i].sin_cos();
        }
    }

    /// Pre-compute the fused pattern from Y-bus structure and bus classification.
    pub fn new(ybus: &YBus, pvpq: &[usize], pq: &[usize]) -> Self {
        let n = ybus.n;
        let n_pvpq = pvpq.len();
        let n_pq = pq.len();
        let dim = n_pvpq + n_pq;

        // Reverse lookup maps
        let mut pvpq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pvpq.iter().enumerate() {
            pvpq_pos[bus] = pos;
        }
        let mut pq_pos = vec![SENTINEL; n];
        for (pos, &bus) in pq.iter().enumerate() {
            pq_pos[bus] = pos;
        }

        let mut indices: Vec<Pair<usize, usize>> = Vec::with_capacity(4 * ybus.nnz);
        let mut bus_ptr = Vec::with_capacity(n + 1);
        let mut off_diag = Vec::with_capacity(4 * ybus.nnz);
        let mut diag = Vec::with_capacity(n);

        let mut triplet_idx = 0usize;

        for i in 0..n {
            bus_ptr.push(off_diag.len());
            let row = ybus.row(i);
            let row_start = ybus.row_start(i);

            let row_h = pvpq_pos[i]; // Jacobian row for H/N (SENTINEL if not in pvpq)
            let row_m = if pq_pos[i] != SENTINEL {
                n_pvpq + pq_pos[i] // Jacobian row for M/L
            } else {
                SENTINEL
            };

            for (k, &j) in row.col_idx.iter().enumerate() {
                let ybus_pos = row_start + k;

                if i == j {
                    // Diagonal entry
                    let mut d = FusedDiag {
                        ybus_pos,
                        h_idx: SENTINEL,
                        n_idx: SENTINEL,
                        m_idx: SENTINEL,
                        l_idx: SENTINEL,
                    };

                    // H_ii (if i is in pvpq)
                    if row_h != SENTINEL {
                        indices.push(Pair::new(row_h, row_h));
                        d.h_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // N_ii (if i is in both pvpq and pq — i.e., i is PQ)
                    if row_h != SENTINEL && pq_pos[i] != SENTINEL {
                        indices.push(Pair::new(row_h, n_pvpq + pq_pos[i]));
                        d.n_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // M_ii (if i is PQ)
                    if row_m != SENTINEL {
                        indices.push(Pair::new(row_m, pvpq_pos[i]));
                        d.m_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // L_ii (if i is PQ)
                    if row_m != SENTINEL {
                        indices.push(Pair::new(row_m, n_pvpq + pq_pos[i]));
                        d.l_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    diag.push(d);
                } else {
                    // Off-diagonal entry
                    let mut entry = FusedOffDiag {
                        j,
                        ybus_pos,
                        h_idx: SENTINEL,
                        n_idx: SENTINEL,
                        m_idx: SENTINEL,
                        l_idx: SENTINEL,
                    };

                    // H_ij (if i in pvpq and j in pvpq)
                    if row_h != SENTINEL && pvpq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_h, pvpq_pos[j]));
                        entry.h_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // N_ij (if i in pvpq and j in pq)
                    if row_h != SENTINEL && pq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_h, n_pvpq + pq_pos[j]));
                        entry.n_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // M_ij (if i in pq and j in pvpq)
                    if row_m != SENTINEL && pvpq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_m, pvpq_pos[j]));
                        entry.m_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    // L_ij (if i in pq and j in pq)
                    if row_m != SENTINEL && pq_pos[j] != SENTINEL {
                        indices.push(Pair::new(row_m, n_pvpq + pq_pos[j]));
                        entry.l_idx = triplet_idx;
                        triplet_idx += 1;
                    }

                    off_diag.push(entry);
                }
            }
        }
        bus_ptr.push(off_diag.len());

        let (symbolic, argsort) =
            SymbolicSparseColMat::<usize>::try_new_from_indices(dim, dim, &indices)
                .expect("Fused Jacobian symbolic pattern construction failed");

        // Extract triplet→CSC mapping by building an identity-valued matrix.
        // This tells us: for each triplet index, what CSC position does it map to?
        let identity_vals: Vec<f64> = (0..triplet_idx).map(|i| i as f64).collect();
        let identity_csc =
            SparseColMat::new_from_argsort(symbolic.clone(), &argsort, &identity_vals)
                .expect("Identity CSC construction failed");
        let csc_vals = identity_csc.as_ref().val();
        let nnz_csc = csc_vals.len();

        // Build inverse mapping: triplet_to_csc[triplet_idx] = csc_position
        let mut triplet_to_csc = vec![SENTINEL; triplet_idx];
        for (csc_pos, &triplet_pos) in csc_vals.iter().enumerate() {
            triplet_to_csc[triplet_pos as usize] = csc_pos;
        }

        // Remap all entry indices from triplet order → CSC order
        for d in &mut diag {
            if d.h_idx != SENTINEL {
                d.h_idx = triplet_to_csc[d.h_idx];
            }
            if d.n_idx != SENTINEL {
                d.n_idx = triplet_to_csc[d.n_idx];
            }
            if d.m_idx != SENTINEL {
                d.m_idx = triplet_to_csc[d.m_idx];
            }
            if d.l_idx != SENTINEL {
                d.l_idx = triplet_to_csc[d.l_idx];
            }
        }
        for e in &mut off_diag {
            if e.h_idx != SENTINEL {
                e.h_idx = triplet_to_csc[e.h_idx];
            }
            if e.n_idx != SENTINEL {
                e.n_idx = triplet_to_csc[e.n_idx];
            }
            if e.m_idx != SENTINEL {
                e.m_idx = triplet_to_csc[e.m_idx];
            }
            if e.l_idx != SENTINEL {
                e.l_idx = triplet_to_csc[e.l_idx];
            }
        }

        Self {
            n,
            dim,
            bus_ptr,
            off_diag,
            diag,
            nnz_csc,
            symbolic,
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

    /// Number of nonzeros in the CSC Jacobian.
    pub fn nnz(&self) -> usize {
        self.nnz_csc
    }

    /// Compute power injections and Jacobian values in a single Y-bus pass.
    ///
    /// Values are written directly in CSC order (no permutation step needed).
    /// Returns (p_calc, q_calc, Jacobian) — allocates new buffers each call.
    /// For zero-allocation iteration, use `build_fused_into_with_trig` instead.
    pub fn build_fused(
        &self,
        ybus: &YBus,
        vm: &[f64],
        va: &[f64],
    ) -> (Vec<f64>, Vec<f64>, SparseColMat<usize, f64>) {
        let mut p_calc = vec![0.0; self.n];
        let mut q_calc = vec![0.0; self.n];
        let mut csc_values = vec![0.0; self.nnz_csc];
        let mut sin_va = vec![0.0f64; self.n];
        let mut cos_va = vec![0.0f64; self.n];

        self.fill_fused(
            ybus,
            vm,
            va,
            &mut p_calc,
            &mut q_calc,
            &mut csc_values,
            &[],
            &[],
            &mut sin_va,
            &mut cos_va,
        );

        let jac = SparseColMat::new(self.symbolic.clone(), csc_values);
        (p_calc, q_calc, jac)
    }

    /// Zero-allocation fused computation using caller-provided trig scratch.
    #[allow(clippy::too_many_arguments)]
    pub fn build_fused_into_with_trig(
        &self,
        ybus: &YBus,
        vm: &[f64],
        va: &[f64],
        p_calc: &mut [f64],
        q_calc: &mut [f64],
        csc_values: &mut [f64],
        zip_dn: &[f64],
        zip_dl: &[f64],
        sin_va: &mut [f64],
        cos_va: &mut [f64],
    ) {
        self.fill_fused(
            ybus, vm, va, p_calc, q_calc, csc_values, zip_dn, zip_dl, sin_va, cos_va,
        );
    }

    /// Fill only Jacobian values (CSC) using pre-computed `p_calc`/`q_calc`
    /// and caller-provided trig scratch buffers.
    ///
    /// Skips the P/Q accumulation pass entirely.  Used after the line search
    /// accepted the full Newton step: `p_calc` and `q_calc` are already valid at
    /// the new operating point (computed by `compute_pq_ybus`), so only the
    /// Jacobian values need to be rebuilt — saving one full Y-bus P/Q traversal
    /// per accepted-step NR iteration.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_jacobian_into_with_trig(
        &self,
        ybus: &YBus,
        vm: &[f64],
        va: &[f64],
        p_calc: &[f64],
        q_calc: &[f64],
        csc_values: &mut [f64],
        zip_dn: &[f64],
        zip_dl: &[f64],
        sin_va: &mut [f64],
        cos_va: &mut [f64],
    ) {
        let has_zip = !zip_dn.is_empty();
        self.fill_angle_trig(va, sin_va, cos_va);
        for i in 0..self.n {
            let vm_i = vm[i];
            let vm_i_safe = vm_i.max(1e-6);
            let si = sin_va[i];
            let ci = cos_va[i];

            let d = &self.diag[i];
            let g_ii = ybus.g_at_pos(d.ybus_pos);
            let b_ii = ybus.b_at_pos(d.ybus_pos);

            // Off-diagonal Jacobian entries
            for entry in &self.off_diag[self.bus_ptr[i]..self.bus_ptr[i + 1]] {
                let j = entry.j;
                let sj = sin_va[j];
                let cj = cos_va[j];
                let sin_t = si * cj - ci * sj;
                let cos_t = ci * cj + si * sj;
                let vm_j = vm[j];
                let g = ybus.g_at_pos(entry.ybus_pos);
                let b = ybus.b_at_pos(entry.ybus_pos);
                let gcos_bsin = g * cos_t + b * sin_t;
                let gsin_bcos = g * sin_t - b * cos_t;

                if entry.h_idx != SENTINEL {
                    csc_values[entry.h_idx] = vm_i * vm_j * gsin_bcos;
                }
                if entry.n_idx != SENTINEL {
                    csc_values[entry.n_idx] = vm_i * gcos_bsin;
                }
                if entry.m_idx != SENTINEL {
                    csc_values[entry.m_idx] = -vm_i * vm_j * gcos_bsin;
                }
                if entry.l_idx != SENTINEL {
                    csc_values[entry.l_idx] = vm_i * gsin_bcos;
                }
            }

            // Diagonal Jacobian entries — use pre-computed p_calc/q_calc
            if d.h_idx != SENTINEL {
                csc_values[d.h_idx] = -q_calc[i] - b_ii * vm_i * vm_i;
            }
            if d.n_idx != SENTINEL {
                csc_values[d.n_idx] = p_calc[i] / vm_i_safe + g_ii * vm_i_safe;
            }
            if d.m_idx != SENTINEL {
                csc_values[d.m_idx] = p_calc[i] - g_ii * vm_i * vm_i;
            }
            if d.l_idx != SENTINEL {
                csc_values[d.l_idx] = q_calc[i] / vm_i_safe - b_ii * vm_i_safe;
            }

            // ZIP load Jacobian corrections: ∂P_load/∂Vm and ∂Q_load/∂Vm
            if has_zip {
                if d.n_idx != SENTINEL {
                    csc_values[d.n_idx] += zip_dn[i];
                }
                if d.l_idx != SENTINEL {
                    csc_values[d.l_idx] += zip_dl[i];
                }
            }
        }
    }

    /// Inner computation: reads G/B values from Y-bus via stored CSR positions.
    ///
    /// The FusedPattern stores structural indices (ybus_pos) that map directly
    /// to positions in the Y-bus's g_vals/b_vals arrays. This allows the pattern
    /// to be reused across contingencies with different Y-bus values — each
    /// contingency applies a delta to the Y-bus, and this method reads the
    /// updated G/B values directly.
    #[allow(clippy::too_many_arguments)]
    fn fill_fused(
        &self,
        ybus: &YBus,
        vm: &[f64],
        va: &[f64],
        p_calc: &mut [f64],
        q_calc: &mut [f64],
        values: &mut [f64],
        zip_dn: &[f64],
        zip_dl: &[f64],
        sin_va: &mut [f64],
        cos_va: &mut [f64],
    ) {
        let has_zip = !zip_dn.is_empty();

        // Zero the output buffers
        p_calc.fill(0.0);
        q_calc.fill(0.0);

        // Precompute sin and cos for every bus angle once.
        // Replaces per-edge sin_cos(va[i]-va[j]) calls with angle-subtraction identities.
        self.fill_angle_trig(va, sin_va, cos_va);

        for i in 0..self.n {
            let vm_i = vm[i];
            // P1-002: Guard against division by zero in Jacobian diagonal entries
            let vm_i_safe = vm_i.max(1e-6);
            let mut p_i = 0.0;
            let mut q_i = 0.0;
            let si = sin_va[i];
            let ci = cos_va[i];

            // Diagonal Y-bus contribution: theta_ii = 0, cos=1, sin=0
            let d = &self.diag[i];
            let g_ii = ybus.g_at_pos(d.ybus_pos);
            let b_ii = ybus.b_at_pos(d.ybus_pos);
            p_i += vm_i * g_ii;
            q_i += -vm_i * b_ii;

            // Off-diagonal entries: use angle-subtraction identities
            for entry in &self.off_diag[self.bus_ptr[i]..self.bus_ptr[i + 1]] {
                let j = entry.j;
                let sj = sin_va[j];
                let cj = cos_va[j];
                let sin_t = si * cj - ci * sj;
                let cos_t = ci * cj + si * sj;
                let vm_j = vm[j];
                let g = ybus.g_at_pos(entry.ybus_pos);
                let b = ybus.b_at_pos(entry.ybus_pos);

                // Common sub-expressions (computed once)
                let gcos_bsin = g * cos_t + b * sin_t;
                let gsin_bcos = g * sin_t - b * cos_t;

                // Mismatch accumulation
                p_i += vm_j * gcos_bsin;
                q_i += vm_j * gsin_bcos;

                // Jacobian off-diagonal fill (directly in CSC order)
                if entry.h_idx != SENTINEL {
                    values[entry.h_idx] = vm_i * vm_j * gsin_bcos;
                }
                if entry.n_idx != SENTINEL {
                    values[entry.n_idx] = vm_i * gcos_bsin;
                }
                if entry.m_idx != SENTINEL {
                    values[entry.m_idx] = -vm_i * vm_j * gcos_bsin;
                }
                if entry.l_idx != SENTINEL {
                    values[entry.l_idx] = vm_i * gsin_bcos;
                }
            }

            // Finalize power injections for bus i
            p_calc[i] = vm_i * p_i;
            q_calc[i] = vm_i * q_i;

            // Diagonal Jacobian entries (depend on final p_calc[i], q_calc[i])
            if d.h_idx != SENTINEL {
                values[d.h_idx] = -q_calc[i] - b_ii * vm_i * vm_i;
            }
            if d.n_idx != SENTINEL {
                // P1-002: Both terms use vm_i_safe so the Jacobian diagonal is
                // consistent with itself at near-zero voltages (vm ≈ 0).
                // N_ii = P_calc/Vm + G_ii·Vm  — using vm_safe for both terms.
                values[d.n_idx] = p_calc[i] / vm_i_safe + g_ii * vm_i_safe;
            }
            if d.m_idx != SENTINEL {
                values[d.m_idx] = p_calc[i] - g_ii * vm_i * vm_i;
            }
            if d.l_idx != SENTINEL {
                // L_ii = Q_calc/Vm - B_ii·Vm  — use vm_safe for both terms (same
                // rationale as N_ii above).
                values[d.l_idx] = q_calc[i] / vm_i_safe - b_ii * vm_i_safe;
            }

            // ZIP load Jacobian corrections: ∂P_load/∂Vm and ∂Q_load/∂Vm
            if has_zip {
                if d.n_idx != SENTINEL {
                    values[d.n_idx] += zip_dn[i];
                }
                if d.l_idx != SENTINEL {
                    values[d.l_idx] += zip_dl[i];
                }
            }
        }
    }
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
    use crate::matrix::jacobian::JacobianPattern;
    use crate::matrix::mismatch::compute_power_injection;
    use crate::matrix::ybus::build_ybus;

    use surge_network::network::BusType;

    fn load_case(name: &str) -> surge_network::Network {
        crate::test_cases::load_case(name)
            .unwrap_or_else(|err| panic!("failed to load {name} fixture: {err}"))
    }

    /// Verify fused results match separate mismatch + Jacobian computation.
    #[test]
    fn test_fused_matches_separate() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        for case_name in &["case9", "case14", "case118"] {
            let net = load_case(case_name);
            let ybus = build_ybus(&net);

            let mut pv = Vec::new();
            let mut pq = Vec::new();
            for (i, bus) in net.buses.iter().enumerate() {
                match bus.bus_type {
                    BusType::PV => pv.push(i),
                    BusType::PQ => pq.push(i),
                    _ => {}
                }
            }
            let mut pvpq: Vec<usize> = pv.iter().chain(pq.iter()).copied().collect();
            pvpq.sort();

            let vm: Vec<f64> = net.buses.iter().map(|b| b.voltage_magnitude_pu).collect();
            let va: Vec<f64> = net.buses.iter().map(|b| b.voltage_angle_rad).collect();

            // Separate computation
            let (p_sep, q_sep) = compute_power_injection(&ybus, &vm, &va);
            let jac_pattern = JacobianPattern::new(&ybus, &pvpq, &pq);
            let jac_sep = jac_pattern.build(&vm, &va, &p_sep, &q_sep);

            // Fused computation
            let fused = FusedPattern::new(&ybus, &pvpq, &pq);
            let (p_fused, q_fused, jac_fused) = fused.build_fused(&ybus, &vm, &va);

            // Compare mismatch (1e-12 tolerance: summation order differs between fused/separate)
            for i in 0..net.n_buses() {
                assert!(
                    (p_sep[i] - p_fused[i]).abs() < 1e-12,
                    "{case_name}: P mismatch at bus {i}: sep={}, fused={}",
                    p_sep[i],
                    p_fused[i]
                );
                assert!(
                    (q_sep[i] - q_fused[i]).abs() < 1e-12,
                    "{case_name}: Q mismatch at bus {i}: sep={}, fused={}",
                    q_sep[i],
                    q_fused[i]
                );
            }

            // Compare Jacobian values (both in CSC format)
            let sep_vals = jac_sep.as_ref().val();
            let fused_vals = jac_fused.as_ref().val();
            assert_eq!(
                sep_vals.len(),
                fused_vals.len(),
                "{case_name}: Jacobian nnz mismatch"
            );

            for k in 0..sep_vals.len() {
                assert!(
                    (sep_vals[k] - fused_vals[k]).abs() < 1e-12,
                    "{case_name}: Jacobian value mismatch at index {k}: sep={}, fused={}",
                    sep_vals[k],
                    fused_vals[k]
                );
            }
        }
    }
}
