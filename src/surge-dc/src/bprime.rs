// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! B' matrix construction for DC power flow.
//!
//! The B' matrix is the imaginary part of the bus admittance matrix (Y-bus)
//! with the slack bus row/column removed. For DC power flow:
//!
//!   B'[i,j] = -b          (off-diagonal, mutual term)
//!   B'[i,i] += b          (from-bus diagonal contribution)
//!   B'[j,j] += b          (to-bus diagonal contribution)
//!
//! where `b = b_series / τ = 1/(x·τ)` for a branch with series reactance x
//! and off-nominal tap ratio τ (1.0 for lines, MATPOWER zero-tap convention).
//!
//! # Tap approximation note (MINOR-2)
//!
//! The exact pi-model DC formulation uses asymmetric diagonal terms:
//!   - from-bus diagonal: `b_series / τ²`
//!   - to-bus diagonal:   `b_series / τ`
//!   - off-diagonal:      `-b_series / τ`
//!
//! This implementation uses `b = 1/(x·τ)` for all three entries, matching
//! MATPOWER `makeBdc`. The error in the from-bus diagonal is O((τ−1)²),
//! approximately 0.3% for a 5% off-nominal tap (τ = 1.05). Correcting this
//! requires a corresponding update to `compute_branch_flows` and the KCL
//! convention (from-bus power ≠ to-bus power for τ ≠ 1), which changes the
//! `branch_p_flow` API. This approximation is retained for internal consistency.
//!
//! The slack bus row and column are removed since its angle is fixed at 0.
//!
//! This module builds a **sparse CSC** representation suitable for KLU factorization.

use std::collections::HashSet;

use surge_network::Network;
use surge_network::network::LccHvdcControlMode;

/// Sparse CSC B' matrix for DC power flow.
pub struct BprimeSparseCsc {
    /// Dimension (n_buses - 1).
    pub dim: usize,
    /// CSC column pointers (length dim+1).
    pub col_ptrs: Vec<usize>,
    /// CSC row indices (length nnz).
    pub row_indices: Vec<usize>,
    /// CSC values (length nnz).
    pub values: Vec<f64>,
    /// Maps internal bus index → reduced index (skipping slack).
    /// Length = n_buses; `None` for the slack bus and excluded buses.
    pub full_to_reduced: Vec<Option<usize>>,
    /// Internal index of the slack bus.
    pub slack_idx: usize,
}

fn reduced_bus_map(
    n: usize,
    slack_idx: usize,
    included_buses: Option<&HashSet<usize>>,
) -> Vec<Option<usize>> {
    let mut full_to_reduced: Vec<Option<usize>> = vec![None; n];
    let mut reduced_idx = 0usize;
    for (i, slot) in full_to_reduced.iter_mut().enumerate() {
        if i == slack_idx {
            continue;
        }
        if included_buses.is_some_and(|buses| !buses.contains(&i)) {
            continue;
        }
        *slot = Some(reduced_idx);
        reduced_idx += 1;
    }
    full_to_reduced
}

/// Build the sparse CSC B' matrix for DC power flow.
///
/// Returns a `BprimeSparseCsc` with the full sparsity structure and values.
#[cfg(test)]
pub fn build_bprime_sparse(network: &Network) -> BprimeSparseCsc {
    let slack_idx = network
        .slack_bus_index()
        .expect("network must have a slack bus");
    build_bprime_sparse_for_buses(network, None, slack_idx)
}

/// Build the sparse CSC B' matrix for a connected AC island of the full network.
///
/// `included_buses` uses global internal bus indices. Any AC branch whose
/// endpoints are not both inside the island is excluded from the matrix.
pub(crate) fn build_bprime_sparse_for_buses(
    network: &Network,
    included_buses: Option<&HashSet<usize>>,
    slack_idx: usize,
) -> BprimeSparseCsc {
    let n = network.n_buses();
    let bus_map = network.bus_index_map();

    // Build mapping from full bus index → reduced index (excluding slack)
    let full_to_reduced = reduced_bus_map(n, slack_idx, included_buses);
    let dim = full_to_reduced.iter().filter(|x| x.is_some()).count();

    // -----------------------------------------------------------------
    // First pass: collect COO triples for all branch contributions.
    // Each branch contributes up to 4 entries (2 diagonal + 2 off-diagonal).
    // -----------------------------------------------------------------
    let mut coo: Vec<(usize, usize, f64)> = Vec::with_capacity(4 * network.branches.len());

    for branch in &network.branches {
        if !branch.in_service {
            continue;
        }
        if branch.x.abs() < crate::types::MIN_REACTANCE {
            continue;
        }

        let from_idx = *bus_map
            .get(&branch.from_bus)
            .expect("from_bus not found in bus map");
        let to_idx = *bus_map
            .get(&branch.to_bus)
            .expect("to_bus not found in bus map");

        if included_buses
            .is_some_and(|buses| !buses.contains(&from_idx) || !buses.contains(&to_idx))
        {
            continue;
        }

        let b = branch.b_dc(); // signed tap-corrected DC susceptance = 1/(x*tap), matches MATPOWER makeBdc

        let ri = full_to_reduced[from_idx];
        let rj = full_to_reduced[to_idx];

        match (ri, rj) {
            (Some(ri), Some(rj)) => {
                // Both non-slack: full off-diagonal + diagonal contributions
                coo.push((ri, ri, b));
                coo.push((rj, rj, b));
                coo.push((ri, rj, -b));
                coo.push((rj, ri, -b));
            }
            (Some(ri), None) => {
                // to_bus is slack: only diagonal contribution at from
                coo.push((ri, ri, b));
            }
            (None, Some(rj)) => {
                // from_bus is slack: only diagonal contribution at to
                coo.push((rj, rj, b));
            }
            (None, None) => {}
        }
    }

    // -----------------------------------------------------------------
    // Sort COO by (col, row) for CSC order, then merge duplicates.
    // -----------------------------------------------------------------
    coo.sort_unstable_by_key(|&(r, c, _)| (c, r));

    let mut col_ptrs = vec![0usize; dim + 1];
    let mut row_indices = Vec::with_capacity(coo.len());
    let mut values = Vec::with_capacity(coo.len());

    let mut cur_col = 0usize;
    col_ptrs[0] = 0;
    for &(r, c, v) in &coo {
        // Advance col_ptrs for empty columns
        while cur_col < c {
            cur_col += 1;
            col_ptrs[cur_col] = row_indices.len();
        }
        // Merge duplicate (row, col) entries
        if let Some(last_row) = row_indices.last() {
            if *last_row == r && col_ptrs[cur_col] < row_indices.len() {
                *values.last_mut().unwrap() += v;
                continue;
            }
        }
        row_indices.push(r);
        values.push(v);
    }
    // Fill remaining column pointers
    while cur_col < dim {
        cur_col += 1;
        col_ptrs[cur_col] = row_indices.len();
    }
    let nnz = row_indices.len();
    col_ptrs[dim] = nnz;

    BprimeSparseCsc {
        dim,
        col_ptrs,
        row_indices,
        values,
        full_to_reduced,
        slack_idx,
    }
}

/// Build the P injection vector for DC power flow (reduced, slack removed).
///
/// P[i] = (sum of Pg at bus i - Pd at bus i - Gs at bus i) / base_mva
///
/// The Gs (shunt conductance) term represents real power consumed by shunt
/// elements to ground, measured in MW at V = 1.0 p.u. In DC power flow the
/// B' matrix contains only branch susceptances (no bus shunts), so this
/// shunt loss must be subtracted from the injection vector explicitly —
/// matching MATPOWER `runpf.m` line 208:
///   `Pbus = real(makeSbus(baseMVA, bus, gen)) - Pbusinj - bus(:, GS) / baseMVA`
///
/// (In AC power flow, Gs is already included in the Y-bus diagonal and is
/// accounted for automatically through P_calc = real(V * conj(Y * V)).)
///
/// If any branches have a non-zero phase shift angle (`shift` field, in degrees),
/// the PST correction is applied to the injection vector:
///
/// For each PST branch (from bus i, to bus j, susceptance b, shift φ radians):
///   P_inj[i] += b * φ    (from-bus: add +b*phi → moves Pbusinj[from] to RHS)
///   P_inj[j] -= b * φ    (to-bus: subtract b*phi → moves Pbusinj[to] to RHS)
///
/// Derivation: the DC B-theta system satisfies bus_flow_sum = Bbus*θ - Pbusinj
/// where Pbusinj = Cft^T * diag(b) * phi (MATPOWER makeBdc convention).
/// Pbusinj[from_k] = +b_k*phi_k, Pbusinj[to_k] = -b_k*phi_k.
/// For bus_flow_sum = Pgen - Pload - Gs to hold, we need Bbus*θ = Pgen - Pload - Gs + Pbusinj.
/// So p[i] = Pgen[i] - Pload[i] - Gs[i] + Pbusinj[i]:
///   p[from] += +b*phi   (adding Pbusinj[from] = +b*phi)
///   p[to]   -= +b*phi   (adding Pbusinj[to] = -b*phi, i.e. subtracting b*phi)
/// This accounts for the "phantom injection" that a phase-shifting transformer
/// introduces into the DC B-theta linear system.
/// Build the reduced injection vector for a connected AC island of the full network.
///
/// `included_buses` uses global internal bus indices. Corrections that touch
/// buses outside the island are ignored for the absent endpoint and still
/// applied for any endpoint inside the island, which is the correct behaviour
/// for fixed-schedule HVDC/MTDC injections spanning multiple AC islands.
pub(crate) fn build_p_injection_for_buses(
    network: &Network,
    full_to_reduced: &[Option<usize>],
    slack_idx: usize,
    included_buses: Option<&HashSet<usize>>,
) -> Vec<f64> {
    let p_full = network.bus_p_injection_pu();
    let n_reduced = full_to_reduced.iter().filter(|x| x.is_some()).count();
    let mut p = vec![0.0; n_reduced];

    for (full_idx, &p_val) in p_full.iter().enumerate() {
        if full_idx == slack_idx {
            continue;
        }
        if included_buses.is_some_and(|buses| !buses.contains(&full_idx)) {
            continue;
        }
        if let Some(ri) = full_to_reduced[full_idx] {
            // Subtract shunt conductance Gs (MW at V=1.0 p.u.) — DC PF has no
            // Y-bus, so this real-power shunt loss must be handled explicitly.
            let gs_pu = network.buses[full_idx].shunt_conductance_mw / network.base_mva;
            p[ri] = p_val - gs_pu;
        }
    }

    // Apply PST corrections: branches with non-zero shift angle.
    let bus_map = network.bus_index_map();
    for branch in &network.branches {
        if !branch.in_service || branch.x.abs() < crate::types::MIN_REACTANCE {
            continue;
        }
        if branch.phase_shift_rad.abs() < 1e-12 {
            continue; // No phase shift — skip.
        }

        let phi_rad = branch.phase_shift_rad;
        let b = branch.b_dc(); // signed susceptance — correct for series-compensated branches
        let correction = b * phi_rad;

        let from_idx = bus_map[&branch.from_bus];
        let to_idx = bus_map[&branch.to_bus];

        // from-bus: add +b*phi (Pbusinj[from] = +b*phi)
        if included_buses.is_none_or(|buses| buses.contains(&from_idx))
            && let Some(ri) = full_to_reduced[from_idx]
        {
            p[ri] += correction;
        }
        // to-bus: subtract b*phi (Pbusinj[to] = -b*phi)
        if included_buses.is_none_or(|buses| buses.contains(&to_idx))
            && let Some(rj) = full_to_reduced[to_idx]
        {
            p[rj] -= correction;
        }
    }

    // Apply DC line (HVDC) injections.
    //
    // Each in-service two-terminal DC line transfers power from the rectifier
    // bus (AC → DC conversion, net load) to the inverter bus (DC → AC
    // conversion, net generation).  Under the DC power flow approximation,
    // losses are neglected and the scheduled MW (`setvl`) is used directly for
    // PowerControl mode, or `setvl * vschd` for CurrentControl mode.
    //
    // This mirrors the fixed-schedule injection applied in AC power flow by
    // `surge_ac::ac_dc::inject_fixed_schedule_dc`, but without reactive power
    // since DC PF ignores Q entirely.
    for dc in network.hvdc.links.iter().filter_map(|link| link.as_lcc()) {
        if dc.mode == LccHvdcControlMode::Blocked {
            continue;
        }
        if !dc.rectifier.in_service || !dc.inverter.in_service {
            continue;
        }

        // Scheduled DC power in MW (active power only for DC approximation).
        //
        // NOTE (current-control voltage floor): The physically correct formula
        // for current-control mode is P = I_dc × V_dc (setvl × vschd).  The
        // vschd.max(1.0) floor below is a guard against bad data (zero or
        // sub-1 pu scheduled voltage) but produces a non-physical result when
        // vschd is legitimately less than 1.0 pu.  Input validation should
        // reject zero/negative vschd before this point; this guard is a
        // last-resort safeguard, not the intended operating regime.
        let p_dc_mw = match dc.mode {
            LccHvdcControlMode::PowerControl => dc.scheduled_setpoint,
            LccHvdcControlMode::CurrentControl => {
                dc.scheduled_setpoint * dc.scheduled_voltage_kv.max(1.0)
            }
            LccHvdcControlMode::Blocked => continue,
        };
        let p_dc_pu = p_dc_mw / network.base_mva;

        // Rectifier bus: consumes AC power (acts as a load → reduce injection).
        if let Some(&from_full) = bus_map.get(&dc.rectifier.bus)
            && included_buses.is_none_or(|buses| buses.contains(&from_full))
            && let Some(ri) = full_to_reduced[from_full]
        {
            p[ri] -= p_dc_pu;
        }
        // If rectifier bus is the slack bus, its injection is not in the
        // reduced system — the slack absorbs the mismatch automatically.

        // Inverter bus: injects AC power (acts as a generator → increase injection).
        if let Some(&to_full) = bus_map.get(&dc.inverter.bus)
            && included_buses.is_none_or(|buses| buses.contains(&to_full))
            && let Some(ri) = full_to_reduced[to_full]
        {
            p[ri] += p_dc_pu;
        }
    }

    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;

    #[test]
    fn test_bprime_case9() {
        skip_if_no_data!();
        let net = load_net("case9");

        let bprime = build_bprime_sparse(&net);

        // Should be 8×8 (9 buses minus 1 slack)
        assert_eq!(bprime.dim, 8);
        assert_eq!(bprime.slack_idx, 0); // bus 1 is slack, internal index 0

        // All column pointer ranges valid
        assert_eq!(bprime.col_ptrs.len(), 9);
        assert_eq!(*bprime.col_ptrs.last().unwrap(), bprime.values.len());

        // Diagonal values should be positive (dominant diagonal)
        for col in 0..bprime.dim {
            let start = bprime.col_ptrs[col];
            let end = bprime.col_ptrs[col + 1];
            let diag_val = bprime.row_indices[start..end]
                .iter()
                .zip(&bprime.values[start..end])
                .find(|&(&r, _)| r == col)
                .map(|(_, &v)| v)
                .unwrap_or(0.0);
            assert!(diag_val > 0.0, "diagonal at col {} should be positive", col);
        }
    }
}
