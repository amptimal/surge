// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Shared support types for DC-SCOPF assembly and screening.

use surge_dc::PtdfRows;

/// Type of contingency cut in the LP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CutType {
    /// LODF-based branch thermal cut.
    BranchThermal,
    /// PTDF-based generator-trip cut.
    GeneratorTrip,
    /// N-2 multi-branch cut (Woodbury rank-2 LODF).
    MultiBranchN2,
}

/// A post-contingency violation found during screening.
pub(crate) struct ViolationInfo {
    pub contingency_idx: usize,
    pub monitored_branch_idx: usize,
    pub severity: f64,
    pub lodf_lk: f64,
}

/// Pre-computed contingency data for LODF calculations.
pub(crate) struct ContingencyData {
    pub ctg_idx: usize,
    pub outaged_br: usize,
    pub from_bus_idx: usize,
    pub to_bus_idx: usize,
    pub denom: f64,
    pub label: String,
}

/// Metadata for a contingency cut row in the sparse QP.
pub(crate) struct CutInfo {
    pub ctg_idx: usize,
    pub monitored_branch_idx: usize,
    pub outaged_branch_indices: Vec<usize>,
    pub lodf_lk: f64,
    pub cut_type: CutType,
    /// For generator-trip cuts: the local gen index (into gen_indices) being tripped.
    pub gen_local_idx: Option<usize>,
}

/// Pre-computed data for a generator contingency.
pub(crate) struct GenContingencyData {
    pub ctg_idx: usize,
    pub gen_local: usize,
    pub bus_idx: usize,
    pub label: String,
}

/// Pre-computed data for an N-2 (two-branch simultaneous) contingency.
pub(crate) struct N2ContingencyData {
    pub ctg_idx: usize,
    pub k1: usize,
    pub k2: usize,
    pub k1_from: usize,
    pub k1_to: usize,
    pub k2_from: usize,
    pub k2_to: usize,
    pub denom_k1: f64,
    pub denom_k2: f64,
    pub lodf_k1k2: f64,
    pub lodf_k2k1: f64,
    pub compound_denom: f64,
}

impl N2ContingencyData {
    /// Compute the N-2 compound LODF coefficients for monitored branch m.
    pub fn compound_lodf(&self, ptdf: &PtdfRows, m: usize) -> (f64, f64) {
        let m_from = get_ptdf(ptdf, m, self.k1_from) - get_ptdf(ptdf, m, self.k1_to);
        let lodf_mk1 = m_from / self.denom_k1;

        let m_from2 = get_ptdf(ptdf, m, self.k2_from) - get_ptdf(ptdf, m, self.k2_to);
        let lodf_mk2 = m_from2 / self.denom_k2;

        let d_m1 = (lodf_mk1 + self.lodf_k1k2 * lodf_mk2) / self.compound_denom;
        let d_m2 = (lodf_mk2 + self.lodf_k2k1 * lodf_mk1) / self.compound_denom;
        (d_m1, d_m2)
    }
}

/// State for a contingency that has been activated in the corrective LP.
pub(crate) struct CorrectiveCtgBlock {
    pub ctg_idx: usize,
    pub theta_k_col_offset: usize,
}

/// Helper: look up PTDF[branch, bus] from the canonical row-backed representation.
#[inline(always)]
pub(crate) fn get_ptdf(ptdf: &PtdfRows, branch: usize, bus: usize) -> f64 {
    ptdf.get(branch, bus)
}
