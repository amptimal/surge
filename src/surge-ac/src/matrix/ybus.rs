// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Bus admittance matrix (Y-bus) construction.
//!
//! Builds the complex sparse Y-bus matrix from the network model.
//! Handles transmission lines (pi-model), transformers (with off-nominal taps
//! and phase shifts), and shunt elements (Gs, Bs).
//!
//! Y-bus formulation follows MATPOWER conventions (makeYbus.m).
//!
//! Storage uses CSR layout with separate real/imaginary arrays for
//! cache-friendly and SIMD-vectorizable access in mismatch and Jacobian
//! computations.

use std::collections::HashMap;

use num_complex::Complex64;
use surge_network::Network;
use surge_network::network::impedance_correction::ImpedanceCorrectionTable;
use surge_network::network::{Branch, Bus};

/// Sparse admittance matrix in CSR format with separate G/B arrays.
///
/// For each row i, the non-zero columns and their conductance/susceptance values
/// are stored in contiguous slices accessible via `row(i)`.
#[derive(Clone)]
pub struct YBus {
    /// Number of buses.
    pub n: usize,
    /// Total number of non-zero entries.
    pub nnz: usize,
    /// Row pointers: row i has entries at indices row_ptr[i]..row_ptr[i+1].
    row_ptr: Vec<usize>,
    /// Column indices (sorted within each row).
    col_idx: Vec<usize>,
    /// Real parts of admittance (conductance G_ij).
    g_vals: Vec<f64>,
    /// Imaginary parts of admittance (susceptance B_ij).
    b_vals: Vec<f64>,
}

/// A view into one row of the Y-bus matrix.
pub struct YBusRow<'a> {
    /// Column indices of the non-zero entries in this row.
    pub col_idx: &'a [usize],
    /// Conductance values (real part of Y) for each non-zero entry.
    pub g: &'a [f64],
    /// Susceptance values (imaginary part of Y) for each non-zero entry.
    pub b: &'a [f64],
}

/// Compute the impedance correction factor for a branch.
///
/// Mirrors the inline computation in `build_ybus_from_parts` so that
/// `branch_removal_delta` applies the same scaling as the original build.
/// Returns 1.0 when the branch has no correction table reference.
fn branch_corr_factor(branch: &Branch, corr_map: &HashMap<u32, &ImpedanceCorrectionTable>) -> f64 {
    branch
        .tab
        .and_then(|t| corr_map.get(&t))
        .map(|tbl| tbl.interpolate(branch.effective_tap()))
        .unwrap_or(1.0)
}

impl YBus {
    /// Get a view of row i (column indices, G values, B values).
    #[inline]
    pub fn row(&self, i: usize) -> YBusRow<'_> {
        let start = self.row_ptr[i];
        let end = self.row_ptr[i + 1];
        YBusRow {
            col_idx: &self.col_idx[start..end],
            g: &self.g_vals[start..end],
            b: &self.b_vals[start..end],
        }
    }

    /// Lookup conductance G\[i,j\]. Returns 0.0 if not present.
    #[inline]
    pub fn g(&self, i: usize, j: usize) -> f64 {
        let row = self.row(i);
        match row.col_idx.binary_search(&j) {
            Ok(pos) => row.g[pos],
            Err(_) => 0.0,
        }
    }

    /// Lookup susceptance B\[i,j\]. Returns 0.0 if not present.
    #[inline]
    pub fn b(&self, i: usize, j: usize) -> f64 {
        let row = self.row(i);
        match row.col_idx.binary_search(&j) {
            Ok(pos) => row.b[pos],
            Err(_) => 0.0,
        }
    }

    /// Lookup complex admittance Y\[i,j\]. Returns 0+0j if not present.
    pub fn at(&self, i: usize, j: usize) -> Complex64 {
        Complex64::new(self.g(i, j), self.b(i, j))
    }

    /// Number of non-zero entries in row i.
    #[inline]
    pub fn row_nnz(&self, i: usize) -> usize {
        self.row_ptr[i + 1] - self.row_ptr[i]
    }

    /// Starting CSR index for row i.
    #[inline]
    pub fn row_start(&self, i: usize) -> usize {
        self.row_ptr[i]
    }

    /// Direct access to conductance value by CSR position.
    #[inline]
    pub fn g_at_pos(&self, pos: usize) -> f64 {
        self.g_vals[pos]
    }

    /// Direct access to susceptance value by CSR position.
    #[inline]
    pub fn b_at_pos(&self, pos: usize) -> f64 {
        self.b_vals[pos]
    }

    /// Find the CSR position of entry (i, j). Returns None if not present.
    #[inline]
    fn find_pos(&self, i: usize, j: usize) -> Option<usize> {
        let start = self.row_ptr[i];
        let end = self.row_ptr[i + 1];
        self.col_idx[start..end]
            .binary_search(&j)
            .ok()
            .map(|pos| start + pos)
    }

    /// Add delta values to entry (i, j) in-place.
    ///
    /// Panics if (i, j) is not in the sparsity pattern.
    #[inline]
    pub fn add_delta(&mut self, i: usize, j: usize, dg: f64, db: f64) {
        let pos = self
            .find_pos(i, j)
            .unwrap_or_else(|| panic!("YBus::add_delta: entry ({i}, {j}) not in sparsity pattern"));
        self.g_vals[pos] += dg;
        self.b_vals[pos] += db;
    }

    /// Compute the Y-bus deltas for removing a single branch.
    ///
    /// Returns 4 deltas: `[(row, col, delta_g, delta_b); 4]` corresponding to
    /// the ff, tt, ft, tf entries that change when the branch is removed.
    /// Apply these deltas (add them) to remove the branch from Y-bus.
    /// Negate and apply them to restore the branch.
    ///
    /// `corr_map` must match the impedance correction tables used when the base
    /// Y-bus was built (i.e. the same `network.metadata.impedance_corrections`).  Pass an
    /// empty map for networks without correction tables.
    pub fn branch_removal_delta(
        branch: &Branch,
        bus_map: &HashMap<u32, usize>,
        corr_map: &HashMap<u32, &ImpedanceCorrectionTable>,
    ) -> [(usize, usize, f64, f64); 4] {
        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];

        // Apply the same impedance correction factor used in build_ybus_from_parts
        // so that the delta exactly cancels the original contribution.
        let corr = branch_corr_factor(branch, corr_map);
        let r_eff = branch.r * corr;
        let x_eff = branch.x * corr;

        // Series admittance (Fix 2 / CRITICAL-02): same clamped formula as
        // build_ybus_from_parts so that delta == rebuild for all branches,
        // including zero-impedance (r=0, x=0) short circuits.
        //
        // Note: r.signum() * 1e-6 evaluates to 0.0 when r==0 (signum returns 0 for zero
        // in IEEE 754), so we check z_sq directly instead of clamping r and x individually.
        // This correctly handles: lossless branches (r=0, x≠0), series capacitors (x<0),
        // and true zero-impedance short circuits (r=0, x=0).
        let z_sq = r_eff * r_eff + x_eff * x_eff;
        let (gs, bs) = if z_sq < 1e-12 {
            // True zero-impedance: represent as very large admittance (short circuit).
            (1e6_f64, -1e6_f64)
        } else {
            (r_eff / z_sq, -x_eff / z_sq)
        };

        // Complex tap ratio (tap=0 means 1.0 in MATPOWER convention)
        let shift_rad = branch.phase_shift_rad;
        let tap = branch.effective_tap();
        let tap_sq = tap * tap;
        let cos_s = shift_rad.cos();
        let sin_s = shift_rad.sin();

        // Y[f,f] -= (ys + g_pi/2 + jb/2) / |tap|^2 + g_mag + j*b_mag  (remove branch + magnetizing shunt)
        let dg_ff = -(gs + branch.g_pi / 2.0) / tap_sq - branch.g_mag;
        let db_ff = -(bs + branch.b / 2.0) / tap_sq - branch.b_mag;

        // Y[t,t] -= ys + g_pi/2 + jb/2
        let dg_tt = -(gs + branch.g_pi / 2.0);
        let db_tt = -(bs + branch.b / 2.0);

        // Y[f,t] += ys / conj(tap_c)  (removing the negative contribution)
        let dg_ft = (gs * cos_s - bs * sin_s) / tap;
        let db_ft = (gs * sin_s + bs * cos_s) / tap;

        // Y[t,f] += ys / tap_c
        let dg_tf = (gs * cos_s + bs * sin_s) / tap;
        let db_tf = (-gs * sin_s + bs * cos_s) / tap;

        [
            (f, f, dg_ff, db_ff),
            (t, t, dg_tt, db_tt),
            (f, t, dg_ft, db_ft),
            (t, f, dg_tf, db_tf),
        ]
    }

    /// Apply a set of deltas to the Y-bus in-place.
    pub fn apply_deltas(&mut self, deltas: &[(usize, usize, f64, f64)]) {
        for &(i, j, dg, db) in deltas {
            self.add_delta(i, j, dg, db);
        }
    }

    /// Reverse a set of deltas (subtract instead of add).
    pub fn unapply_deltas(&mut self, deltas: &[(usize, usize, f64, f64)]) {
        for &(i, j, dg, db) in deltas {
            self.add_delta(i, j, -dg, -db);
        }
    }
}

/// Build the Y-bus admittance matrix from a network.
///
/// Convenience wrapper around [`build_ybus_from_parts`].
pub fn build_ybus(network: &Network) -> YBus {
    let bus_map = network.bus_index_map();
    build_ybus_from_parts(
        &network.branches,
        &network.buses,
        network.base_mva,
        &bus_map,
        &network.metadata.impedance_corrections,
    )
}

/// Build the Y-bus admittance matrix from individual components.
///
/// This is useful when branches have been modified (e.g. contingency analysis)
/// without cloning the entire Network.
///
/// For each branch (pi-model with optional transformer):
///   - Series admittance: ys = 1 / (r + jx)
///   - Line charging: bc = jb (total, split half per end)
///   - Line charging conductance: g_pi (total, split half per end)
///   - Complex tap: tap_c = tap * exp(j * shift_rad)
///
///   Y\[f,f\] += (ys + g_pi/2 + jb/2) / |tap|^2
///   Y\[t,t\] += ys + g_pi/2 + jb/2
///   Y\[f,t\] -= ys / conj(tap_c)
///   Y\[t,f\] -= ys / tap_c
///
/// For bus shunts:
///   Y\[i,i\] += (gs + j*bs) / baseMVA
pub fn build_ybus_from_parts(
    branches: &[Branch],
    buses: &[Bus],
    base_mva: f64,
    bus_map: &HashMap<u32, usize>,
    impedance_corrections: &[ImpedanceCorrectionTable],
) -> YBus {
    let n = buses.len();

    // Build a lookup from table number → table reference (O(1) per branch).
    let corr_map: HashMap<u32, &ImpedanceCorrectionTable> = impedance_corrections
        .iter()
        .map(|t| (t.number, t))
        .collect();

    // Phase 1: Collect raw COO triplets (row, col, G, B) without deduplication.
    // A flat Vec + sort_unstable + merge is faster than HashMap for power-system
    // sizes because it avoids hash computation, bucket allocation, and collision
    // handling.  Each in-service branch contributes ≤5 entries (ff, tt, ft, tf,
    // plus an optional magnetizing shunt at ff).  All-diagonal ensures are pushed
    // as (i, i, 0, 0) and will be folded in during the merge pass.
    let n_br_est = branches.len();
    let mut triplets: Vec<(usize, usize, f64, f64)> =
        Vec::with_capacity(5 * n_br_est + buses.len() + n);

    let j_imag = Complex64::new(0.0, 1.0);

    // Add branch contributions
    for branch in branches {
        if !branch.in_service {
            continue;
        }

        let f = bus_map[&branch.from_bus];
        let t = bus_map[&branch.to_bus];

        // Apply impedance correction table if referenced by this branch (PSS/E TAB1).
        // The correction factor F(tap) scales both R and X before admittance computation.
        let corr = branch_corr_factor(branch, &corr_map);
        let (r_eff, x_eff) = (branch.r * corr, branch.x * corr);

        // Series admittance (Fix 2 / CRITICAL-02): zero-impedance branches use
        // a large-admittance short-circuit substitution.  Computed directly from
        // z² to correctly handle: lossless branches (r=0, x≠0), series capacitors
        // (x<0), and true zero-impedance branches (r=0, x=0).
        //
        // Note: r.signum() * 1e-6 evaluates to 0.0 when r==0 (signum returns 0 for
        // zero in IEEE 754), which is why we avoid the individual-component clamp
        // and check z_sq directly.
        let z_sq = r_eff * r_eff + x_eff * x_eff;
        let ys = if z_sq < 1e-12 {
            Complex64::new(1e6_f64, -1e6_f64)
        } else {
            Complex64::new(1.0, 0.0) / Complex64::new(r_eff, x_eff)
        };

        // Line charging susceptance (total) and conductance (total)
        let bc = j_imag * branch.b;

        // Complex tap ratio (Fix 1 / CRITICAL-01): MATPOWER exports tap=0 to
        // mean tap=1 (no transformation).  Without this guard, division by zero
        // injects Infinity into Y-bus.  Consistent with branch_removal_delta.
        let tap_raw = branch.effective_tap();
        if tap_raw < 0.0 {
            tracing::warn!(
                from_bus = branch.from_bus,
                to_bus = branch.to_bus,
                tap = tap_raw,
                "negative tap ratio in Y-bus build: transformer polarity will be inverted"
            );
        }
        let shift_rad = branch.phase_shift_rad;
        let tap_c = Complex64::new(tap_raw * shift_rad.cos(), tap_raw * shift_rad.sin());
        let tap_mag_sq = tap_raw * tap_raw;

        // Y[f,f] += (ys + g_pi/2 + bc/2) / |tap|^2
        // g_pi/2 adds to the real (conductance) part of the from-bus diagonal shunt,
        // scaled by tap^2 the same way bc/2 is (pi-circuit from-end shunt convention).
        let yff = (ys + Complex64::new(branch.g_pi / 2.0, 0.0) + bc / 2.0) / tap_mag_sq;
        triplets.push((f, f, yff.re, yff.im));

        // Y[t,t] += ys + g_pi/2 + bc/2
        let ytt = ys + Complex64::new(branch.g_pi / 2.0, 0.0) + bc / 2.0;
        triplets.push((t, t, ytt.re, ytt.im));

        // Y[f,t] -= ys / conj(tap_c)
        let yft = -ys / tap_c.conj();
        triplets.push((f, t, yft.re, yft.im));

        // Y[t,f] -= ys / tap_c
        let ytf = -ys / tap_c;
        triplets.push((t, f, ytf.re, ytf.im));

        // Transformer magnetizing admittance — shunt at winding-1 (from_bus) terminal.
        // PSS/E MAG1/MAG2 are in p.u. on the system MVA base; add directly to Y[f,f].
        if branch.g_mag.abs() > 1e-12 || branch.b_mag.abs() > 1e-12 {
            triplets.push((f, f, branch.g_mag, branch.b_mag));
        }
    }

    // Add bus shunt contributions: Y[i,i] += (gs + j*bs) / baseMVA
    for (i, bus) in buses.iter().enumerate() {
        if bus.shunt_conductance_mw != 0.0 || bus.shunt_susceptance_mvar != 0.0 {
            triplets.push((
                i,
                i,
                bus.shunt_conductance_mw / base_mva,
                bus.shunt_susceptance_mvar / base_mva,
            ));
        }
    }

    // Fixed shunts preserve equipment identity for topology remap and I/O, but
    // their electrical contribution must already be reflected in the bus shunt
    // totals. Keeping buses authoritative avoids double-counting in solvers
    // that read bus shunts directly instead of reconstructing from devices.

    // Ensure all diagonal entries exist (even if zero, for Jacobian diagonal terms).
    // These zero-value entries will be merged without changing any accumulated value.
    for i in 0..n {
        triplets.push((i, i, 0.0, 0.0));
    }

    // Phase 2: Sort then merge adjacent same-key triplets into CSR arrays.
    // sort_unstable_by_key is O(k log k) and avoids the allocator overhead of HashMap.
    triplets.sort_unstable_by_key(|&(r, c, _, _)| (r, c));

    let cap = triplets.len(); // upper bound on nnz (before merge)
    let mut row_ptr = vec![0usize; n + 1];
    let mut col_idx: Vec<usize> = Vec::with_capacity(cap);
    let mut g_vals: Vec<f64> = Vec::with_capacity(cap);
    let mut b_vals: Vec<f64> = Vec::with_capacity(cap);

    let mut iter = triplets.into_iter();
    if let Some((mut cr, mut cc, mut cg, mut cb)) = iter.next() {
        for (r, c, g, b) in iter {
            if r == cr && c == cc {
                // Same (row, col): accumulate into current entry
                cg += g;
                cb += b;
            } else {
                // New (row, col): commit current entry
                row_ptr[cr + 1] += 1;
                col_idx.push(cc);
                g_vals.push(cg);
                b_vals.push(cb);
                cr = r;
                cc = c;
                cg = g;
                cb = b;
            }
        }
        // Commit the final accumulated entry
        row_ptr[cr + 1] += 1;
        col_idx.push(cc);
        g_vals.push(cg);
        b_vals.push(cb);
    }

    let nnz = col_idx.len();

    // Cumulative sum for row pointers
    for i in 1..=n {
        row_ptr[i] += row_ptr[i - 1];
    }

    YBus {
        n,
        nnz,
        row_ptr,
        col_idx,
        g_vals,
        b_vals,
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

    fn load_case(name: &str) -> Network {
        crate::test_cases::load_case(name)
            .unwrap_or_else(|err| panic!("failed to load {name} fixture: {err}"))
    }

    #[test]
    fn test_ybus_case9() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let ybus = build_ybus(&net);

        assert_eq!(ybus.n, 9);

        // Sparsity: case9 has 9 buses, 9 branches → expect ~27 non-zeros
        // (9 diagonal + 9*2 off-diagonal)
        assert!(ybus.nnz > 0 && ybus.nnz < 9 * 9, "nnz={}", ybus.nnz);

        // Y-bus should be symmetric for networks without phase shifters
        for i in 0..9 {
            for j in 0..9 {
                let diff = (ybus.at(i, j) - ybus.at(j, i)).norm();
                assert!(diff < 1e-10, "Y-bus not symmetric at ({}, {})", i, j);
            }
        }

        // Diagonal elements should have positive real part (conductance)
        for i in 0..9 {
            assert!(ybus.g(i, i) >= 0.0, "G[{i},{i}] should be non-negative");
        }

        // Off-diagonal elements between connected buses should be non-zero
        // Bus 1 (idx 0) connects to bus 4 (idx 3)
        assert!(ybus.at(0, 3).norm() > 0.0, "Y[0,3] should be non-zero");

        // Unconnected buses should have zero off-diagonal
        // Bus 1 (idx 0) does not connect to bus 2 (idx 1)
        assert!(ybus.at(0, 1).norm() < 1e-10, "Y[0,1] should be zero");
    }

    #[test]
    fn test_ybus_case14_transformers() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case14");
        let ybus = build_ybus(&net);

        assert_eq!(ybus.n, 14);

        // Case14 has transformers — Y-bus should still be symmetric
        // (no phase shifters in case14, just off-nominal taps)
        for i in 0..14 {
            for j in 0..14 {
                let diff = (ybus.at(i, j) - ybus.at(j, i)).norm();
                assert!(
                    diff < 1e-8,
                    "Y-bus not symmetric at ({}, {}): {} vs {}",
                    i,
                    j,
                    ybus.at(i, j),
                    ybus.at(j, i)
                );
            }
        }

        // Bus 9 (idx 8) has Bs=19 — should contribute to diagonal
        let bus9_shunt = Complex64::new(0.0, 19.0) / 100.0;
        assert!(
            ybus.b(8, 8).abs() > bus9_shunt.im.abs(),
            "Bus 9 diagonal B should include shunt"
        );
    }

    #[test]
    fn test_ybus_row_sum() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case9");
        let ybus = build_ybus(&net);

        // Verify diagonal dominance
        for i in 0..9 {
            let diag = ybus.at(i, i).norm();
            let row = ybus.row(i);
            let off_diag_sum: f64 = row
                .col_idx
                .iter()
                .enumerate()
                .filter(|&(_, &j)| j != i)
                .map(|(k, _)| Complex64::new(row.g[k], row.b[k]).norm())
                .sum();
            assert!(
                diag >= off_diag_sum * 0.5,
                "Bus {} not diagonally dominant enough",
                i
            );
        }
    }

    #[test]
    fn test_ybus_branch_removal_delta() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        // Verify that Y-bus delta matches full rebuild with branch removed
        for case_name in &["case9", "case14", "case118"] {
            let net = load_case(case_name);
            let bus_map = net.bus_index_map();
            let base_ybus = build_ybus(&net);

            for (br_idx, branch) in net.branches.iter().enumerate() {
                if !branch.in_service {
                    continue;
                }

                // Method 1: Delta approach — clone base Y-bus, apply delta
                let corr_map: HashMap<u32, &ImpedanceCorrectionTable> = net
                    .metadata
                    .impedance_corrections
                    .iter()
                    .map(|t| (t.number, t))
                    .collect();
                let deltas = YBus::branch_removal_delta(branch, &bus_map, &corr_map);
                let mut delta_ybus = base_ybus.clone();
                delta_ybus.apply_deltas(&deltas);

                // Method 2: Full rebuild with branch disabled
                let mut branches_mod = net.branches.clone();
                branches_mod[br_idx].in_service = false;
                let rebuild_ybus =
                    build_ybus_from_parts(&branches_mod, &net.buses, net.base_mva, &bus_map, &[]);

                // Compare all entries
                let n = net.n_buses();
                for i in 0..n {
                    for j in 0..n {
                        let dg = delta_ybus.g(i, j);
                        let rg = rebuild_ybus.g(i, j);
                        let db = delta_ybus.b(i, j);
                        let rb = rebuild_ybus.b(i, j);
                        assert!(
                            (dg - rg).abs() < 1e-10 && (db - rb).abs() < 1e-10,
                            "{case_name} branch {br_idx}: Y[{i},{j}] delta=({dg:.6},{db:.6}) rebuild=({rg:.6},{rb:.6})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_ybus_g_pi_branch_conductance() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        // Verify that g_pi (line charging conductance) adds g_pi/2 to both diagonal
        // entries but does NOT change any off-diagonal entries.
        //
        // 2-bus network: one branch with g_pi = 0.02 pu (typical cable value).
        use std::collections::HashMap;
        use surge_network::network::{Branch, Bus, BusType};

        let buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PQ, 100.0),
        ];
        let mut bus_map = HashMap::new();
        bus_map.insert(1u32, 0usize);
        bus_map.insert(2u32, 1usize);

        let r = 0.01_f64;
        let x = 0.1_f64;
        let b = 0.05_f64;
        let g_pi_val = 0.02_f64;

        // Branch without g_pi
        let mut br_base = Branch::new_line(1, 2, r, x, b);
        br_base.g_pi = 0.0;

        // Branch with g_pi
        let mut br_gpi = Branch::new_line(1, 2, r, x, b);
        br_gpi.g_pi = g_pi_val;

        let ybus_base = build_ybus_from_parts(&[br_base], &buses, 100.0, &bus_map, &[]);
        let ybus_gpi = build_ybus_from_parts(&[br_gpi], &buses, 100.0, &bus_map, &[]);

        // Diagonal real parts should increase by g_pi/2 at each bus
        let expected_delta = g_pi_val / 2.0;
        let delta_g00 = ybus_gpi.g(0, 0) - ybus_base.g(0, 0);
        let delta_g11 = ybus_gpi.g(1, 1) - ybus_base.g(1, 1);
        assert!(
            (delta_g00 - expected_delta).abs() < 1e-12,
            "G[0,0] should increase by g_pi/2={expected_delta:.6}; got delta={delta_g00:.6}"
        );
        assert!(
            (delta_g11 - expected_delta).abs() < 1e-12,
            "G[1,1] should increase by g_pi/2={expected_delta:.6}; got delta={delta_g11:.6}"
        );

        // Diagonal imaginary (susceptance) parts should be unchanged
        let delta_b00 = ybus_gpi.b(0, 0) - ybus_base.b(0, 0);
        let delta_b11 = ybus_gpi.b(1, 1) - ybus_base.b(1, 1);
        assert!(
            delta_b00.abs() < 1e-12,
            "B[0,0] should not change; got delta={delta_b00:.6}"
        );
        assert!(
            delta_b11.abs() < 1e-12,
            "B[1,1] should not change; got delta={delta_b11:.6}"
        );

        // Off-diagonal entries must be unchanged (g_pi is a shunt, not series)
        let delta_g01 = ybus_gpi.g(0, 1) - ybus_base.g(0, 1);
        let delta_g10 = ybus_gpi.g(1, 0) - ybus_base.g(1, 0);
        assert!(
            delta_g01.abs() < 1e-12,
            "G[0,1] should not change; got delta={delta_g01:.6}"
        );
        assert!(
            delta_g10.abs() < 1e-12,
            "G[1,0] should not change; got delta={delta_g10:.6}"
        );
    }

    #[test]
    fn test_ybus_branch_removal_delta_pst() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        // MAJOR-26: Regression test for branch_removal_delta with an active PST.
        //
        // Constructs a 3-bus network with one PST branch (shift = 5.0 degrees),
        // verifies that the delta approach produces the same Y-bus as a full
        // rebuild after branch removal, to within 1e-12.
        use std::collections::HashMap;
        use surge_network::network::{Branch, Bus, BusType};

        let buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PV, 100.0),
            Bus::new(3, BusType::PQ, 100.0),
        ];
        let mut bus_map = HashMap::new();
        bus_map.insert(1u32, 0usize);
        bus_map.insert(2u32, 1usize);
        bus_map.insert(3u32, 2usize);

        // Ordinary line: bus 1 → bus 2
        let mut br_line = Branch::new_line(1, 2, 0.01, 0.08, 0.04);
        br_line.tap = 1.0;
        br_line.phase_shift_rad = 0.0;

        // PST branch: bus 2 → bus 3, shift = 5.0 degrees, off-nominal tap
        let mut br_pst = Branch::new_line(2, 3, 0.02, 0.12, 0.06);
        br_pst.tap = 1.05;
        br_pst.phase_shift_rad = 5.0_f64.to_radians();

        // Another ordinary line: bus 1 → bus 3
        let mut br_line2 = Branch::new_line(1, 3, 0.03, 0.15, 0.05);
        br_line2.tap = 1.0;
        br_line2.phase_shift_rad = 0.0;

        let branches = vec![br_line.clone(), br_pst.clone(), br_line2.clone()];

        // Full Y-bus with all branches in service
        let base_ybus = build_ybus_from_parts(&branches, &buses, 100.0, &bus_map, &[]);

        // Compute delta for removing the PST branch (index 1)
        let empty_corr_map: HashMap<u32, &ImpedanceCorrectionTable> = HashMap::new();
        let deltas = YBus::branch_removal_delta(&br_pst, &bus_map, &empty_corr_map);
        let mut delta_ybus = base_ybus.clone();
        delta_ybus.apply_deltas(&deltas);

        // Full rebuild with PST branch disabled
        let mut branches_mod = branches.clone();
        branches_mod[1].in_service = false;
        let rebuild_ybus = build_ybus_from_parts(&branches_mod, &buses, 100.0, &bus_map, &[]);

        // Assert: delta matches full rebuild to within 1e-12
        let n = buses.len();
        for i in 0..n {
            for j in 0..n {
                let dg = delta_ybus.g(i, j);
                let rg = rebuild_ybus.g(i, j);
                let db = delta_ybus.b(i, j);
                let rb = rebuild_ybus.b(i, j);
                assert!(
                    (dg - rg).abs() < 1e-12 && (db - rb).abs() < 1e-12,
                    "PST branch removal delta mismatch at Y[{i},{j}]: \
                     delta=({dg:.8e},{db:.8e}) rebuild=({rg:.8e},{rb:.8e})"
                );
            }
        }
    }

    #[test]
    fn test_ybus_branch_removal_delta_with_impedance_correction() {
        // Verify that branch_removal_delta with a correction table exactly cancels
        // the original Y-bus contribution, even when the transformer has tab != None.
        use surge_network::network::{Branch, Bus, BusType};

        let buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PQ, 100.0),
        ];
        let mut bus_map = HashMap::new();
        bus_map.insert(1u32, 0usize);
        bus_map.insert(2u32, 1usize);

        // Transformer branch with tab=1 so the correction table is applied.
        let mut br = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br.tap = 1.05;
        br.phase_shift_rad = 0.0;
        br.tab = Some(1);

        // Correction table: two entries → at tap=1.05, F = interpolate(1.0→1.0, 1.1→1.3) = 1.15
        let tbl = ImpedanceCorrectionTable {
            number: 1,
            entries: vec![(1.0, 1.0), (1.1, 1.3)],
        };
        let impedance_corrections = vec![tbl];

        // Build base Y-bus WITH correction table
        let base_ybus = build_ybus_from_parts(
            &[br.clone()],
            &buses,
            100.0,
            &bus_map,
            &impedance_corrections,
        );

        // Compute delta using the same correction table
        let corr_map: HashMap<u32, &ImpedanceCorrectionTable> = impedance_corrections
            .iter()
            .map(|t| (t.number, t))
            .collect();
        let deltas = YBus::branch_removal_delta(&br, &bus_map, &corr_map);
        let mut delta_ybus = base_ybus.clone();
        delta_ybus.apply_deltas(&deltas);

        // Build reference Y-bus with the branch disabled
        let mut br_off = br.clone();
        br_off.in_service = false;
        let ref_ybus =
            build_ybus_from_parts(&[br_off], &buses, 100.0, &bus_map, &impedance_corrections);

        // Every entry in delta_ybus must match ref_ybus to 1e-12
        let n = buses.len();
        for i in 0..n {
            for j in 0..n {
                let dg = delta_ybus.g(i, j);
                let rg = ref_ybus.g(i, j);
                let db = delta_ybus.b(i, j);
                let rb = ref_ybus.b(i, j);
                assert!(
                    (dg - rg).abs() < 1e-12 && (db - rb).abs() < 1e-12,
                    "impedance-correction delta mismatch at Y[{i},{j}]: \
                     delta=({dg:.8e},{db:.8e}) ref=({rg:.8e},{rb:.8e})"
                );
            }
        }
    }

    #[test]
    fn test_ybus_sparsity() {
        if !data_available() {
            eprintln!(
                "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
            );
            return;
        }
        let net = load_case("case118");
        let ybus = build_ybus(&net);

        assert_eq!(ybus.n, 118);

        // 118 buses, 186 branches → expect much less than 118² = 13924
        // Should be ~118 (diag) + 2*186 (off-diag) = 490 non-zeros
        let dense_nnz = 118 * 118;
        assert!(
            ybus.nnz < dense_nnz / 10,
            "sparse Y-bus should be << dense: nnz={} vs dense={}",
            ybus.nnz,
            dense_nnz
        );

        // Every row should have at least one entry (the diagonal)
        for i in 0..118 {
            assert!(ybus.row_nnz(i) >= 1, "row {} has no entries", i);
        }
    }
}
