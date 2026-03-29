// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power mismatch computation for AC power flow.
//!
//! Computes the difference between specified and calculated power injections
//! at each bus. Convergence is achieved when the maximum mismatch is below
//! the specified tolerance.

use crate::matrix::ybus::YBus;

/// Compute real and reactive power injections from the Y-bus and voltage state.
///
/// P_i = Vm_i * sum_j Vm_j * (G_ij * cos(θ_ij) + B_ij * sin(θ_ij))
/// Q_i = Vm_i * sum_j Vm_j * (G_ij * sin(θ_ij) - B_ij * cos(θ_ij))
///
/// Only iterates over non-zero Y-bus entries (sparse row iteration).
pub fn compute_power_injection(ybus: &YBus, vm: &[f64], va: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = ybus.n;
    let mut p = vec![0.0; n];
    let mut q = vec![0.0; n];

    // Precompute per-bus sin/cos once (n calls) then use angle-subtraction identities
    // in the inner loop instead of per-edge sin_cos(va[i]-va[j]) calls.
    let mut sin_va = vec![0.0f64; n];
    let mut cos_va = vec![0.0f64; n];
    for i in 0..n {
        (sin_va[i], cos_va[i]) = va[i].sin_cos();
    }

    for i in 0..n {
        let row = ybus.row(i);
        let mut p_i = 0.0;
        let mut q_i = 0.0;
        let si = sin_va[i];
        let ci = cos_va[i];

        for (k, &j) in row.col_idx.iter().enumerate() {
            let g_ij = row.g[k];
            let b_ij = row.b[k];
            let sj = sin_va[j];
            let cj = cos_va[j];
            // sin(a-b) = sin(a)cos(b) - cos(a)sin(b)
            // cos(a-b) = cos(a)cos(b) + sin(a)sin(b)
            let sin_t = si * cj - ci * sj;
            let cos_t = ci * cj + si * sj;

            p_i += vm[j] * (g_ij * cos_t + b_ij * sin_t);
            q_i += vm[j] * (g_ij * sin_t - b_ij * cos_t);
        }

        p[i] = vm[i] * p_i;
        q[i] = vm[i] * q_i;
    }

    (p, q)
}
