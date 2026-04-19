// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! LP variable layout and constraint block types for dispatch formulations.

use surge_sparse::Triplet;

/// Named variable offsets for a single dispatch period.
///
/// For SCED these are absolute column indices (`col_base = 0`).
/// For SCUC these are *relative* within one hour; the absolute column is
/// `col_base + offset` where `col_base = t * n_vars`.
///
/// Fields are filled by each solver after it inserts its solver-specific
/// variables (e.g. SCUC inserts commitment binaries before storage).
/// Constraint builders use the named fields and never assume ordering.
#[allow(dead_code)] // reserved for future SCUC wiring
#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchOffsets {
    /// Bus voltage angle variables θ[0..n_bus).
    pub theta: usize,
    /// Generator dispatch variables Pg[0..n_gen).
    pub pg: usize,
    /// Storage charge variables ch[0..n_storage).
    pub sto_ch: usize,
    /// Storage discharge variables dis[0..n_storage).
    pub sto_dis: usize,
    /// Storage state-of-charge variables soc[0..n_soc).
    /// SCED and SCUC both use explicit soc variables when storage is present.
    pub sto_soc: usize,
    /// Storage discharge offer epiograph variables e_dis[0..n_sto_dis_epi).
    pub sto_epi_dis: usize,
    /// Storage charge bid epiograph variables e_ch[0..n_sto_ch_epi).
    pub sto_epi_ch: usize,
    /// HVDC link dispatch variables P_hvdc[0..n_hvdc_vars).
    pub hvdc: usize,
    /// Generator PWL epiograph variables e_g[0..n_pwl_gen).
    pub e_g: usize,
    /// Dispatchable load variables P_dl[0..n_dl).
    pub dl: usize,
    /// Virtual bid variables vbid[0..n_vbid).
    pub vbid: usize,
    /// Dispatch block variables Δ[0..n_block_vars) (DISP-PWR).
    pub block: usize,
    /// Generic reserve variable block base.
    pub reserve: usize,
    /// Per-block reserve variable offset (DISP-PWR per-block reserves).
    pub block_reserve: usize,
    /// Total variables per period.  SCUC: `col_base = t * n_vars`.
    pub n_vars: usize,
}

/// A self-contained LP constraint block returned by shared builders.
///
/// Row indices inside `triplets` and in `row_lower`/`row_upper` are
/// absolute (i.e. they already incorporate the `row_base` passed to
/// the builder).
#[derive(Default)]
pub(crate) struct LpBlock {
    pub triplets: Vec<Triplet<f64>>,
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,
}

impl LpBlock {
    pub fn empty() -> Self {
        Self::default()
    }

    #[allow(dead_code)] // reserved for future use
    pub fn n_rows(&self) -> usize {
        self.row_lower.len()
    }

    /// Append this block's data into the master LP accumulation vectors.
    pub fn extend_into(
        self,
        all_triplets: &mut Vec<Triplet<f64>>,
        all_lo: &mut Vec<f64>,
        all_hi: &mut Vec<f64>,
    ) {
        all_triplets.extend(self.triplets);
        all_lo.extend(self.row_lower);
        all_hi.extend(self.row_upper);
    }

    /// Copy this block into preallocated row-bound arrays.
    ///
    /// `triplets` already store absolute row indices from the builder's `row_base`.
    /// `row_base` here selects where the local `row_lower`/`row_upper` vectors land
    /// in the caller's preallocated bound arrays.
    pub fn write_into_preallocated(
        self,
        all_triplets: &mut Vec<Triplet<f64>>,
        all_lo: &mut [f64],
        all_hi: &mut [f64],
        row_base: usize,
    ) {
        let n_rows = self.row_lower.len();
        all_triplets.extend(self.triplets);
        all_lo[row_base..row_base + n_rows].copy_from_slice(&self.row_lower);
        all_hi[row_base..row_base + n_rows].copy_from_slice(&self.row_upper);
    }
}
