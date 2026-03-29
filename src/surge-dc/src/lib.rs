// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge DC — DC power flow solver and linear sensitivity analysis.
//!
//! # DC Power Flow: Fundamental Assumptions
//!
//! The DC power flow model makes **three simultaneous approximations** to linearize
//! the AC power flow equations:
//!
//! 1. **Flat voltage profile** — all bus voltage magnitudes are assumed to be 1.0 p.u.
//!    (`|V_i| = 1.0` for all buses). This eliminates the voltage magnitude unknowns
//!    and decouples P from Q.
//!
//! 2. **Small angle differences** — the sine of the voltage angle difference across
//!    each branch is replaced by the angle itself: `sin(theta_i - theta_j) ≈ theta_i - theta_j`.
//!    Valid when angle differences are small (typically < 10-15 degrees).
//!
//! 3. **Lossless branches** — branch resistance is neglected (`r ≈ 0`, so that
//!    `g_ij ≈ 0` and the branch admittance is purely imaginary: `y_ij ≈ -j/x_ij`).
//!    This means real power losses (I^2 R) are not captured.
//!
//! Together, these yield the linear system:
//!
//! ```text
//! P = B' * theta
//! ```
//!
//! where `B'` is the bus susceptance matrix (with the slack bus row/column removed)
//! and `theta` is the vector of bus voltage angles. This system is solved with a
//! single sparse LU factorization (KLU) — no iteration required.
//!
//! # What DC Power Flow Does NOT Compute
//!
//! Because of the three approximations above, DC power flow **does not** produce:
//!
//! - **Voltage magnitudes** — reported as 1.0 p.u. for all buses (assumed, not computed).
//! - **Reactive power flows** — reported as 0.0 for all buses and branches.
//! - **Real power losses** — the lossless assumption means branch losses are zero.
//! - **Voltage-dependent load behavior** — all loads are treated as constant-power at 1.0 p.u.
//!
//! For precise power flow results including losses, reactive power, voltage magnitudes,
//! and voltage-dependent loads, use the AC Newton-Raphson solver in `surge_ac`.
//!
//! # When to Use DC Power Flow
//!
//! DC power flow is the **industry standard** for applications where speed and linearity
//! matter more than exact voltage/reactive results:
//!
//! - **ISO/RTO real-time market clearing** — all US ISOs (ERCOT, PJM, MISO, CAISO, SPP,
//!   NYISO, ISO-NE) use DC power flow in their Security-Constrained Economic Dispatch (SCED)
//!   and Locational Marginal Price (LMP) engines.
//! - **Contingency screening** — PTDF/LODF-based N-1 and N-2 screening is orders of
//!   magnitude faster than AC re-solve for each contingency.
//! - **Transfer capability studies** — ATC/TTC/AFC calculations per NERC methodology.
//! - **Shift factor computation** — GSF, BLDF, PTDF, LODF for market and planning analysis.
//! - **Injection capability heatmaps** — FERC Order 2023 interconnection study requirements.
//!
//! # Module Structure
//!
//! - Top-level exports provide the canonical public API.
//! - [`streaming`] exposes advanced lazy LODF/N-2 column builders.
//! - Internal modules cover the DC solver kernel and one-shot sensitivity wrappers.
//!
//! Transfer capability analysis (ATC/TTC/AFC, DFAX, stability-limited transfer)
//! has been consolidated in the `surge_transfer` crate.

pub(crate) mod bprime;
mod sensitivity;
mod solver;
#[cfg(test)]
mod test_util;
pub(crate) mod types;

/// Lazy streaming builders for LODF and N-2 LODF column computation.
pub mod streaming {
    pub use crate::solver::{LodfColumnBuilder, N2LodfColumnBuilder};
}

pub use sensitivity::{
    DcSensitivityOptions, DcSensitivitySlack, LodfMatrixRequest, LodfRequest, N2LodfBatchRequest,
    N2LodfRequest, OtdfRequest, PtdfRequest, compute_lodf, compute_lodf_matrix, compute_lodf_pairs,
    compute_n2_lodf, compute_n2_lodf_batch, compute_otdf, compute_ptdf,
};
pub use solver::{PreparedDcStudy, run_dc_analysis, solve_dc, solve_dc_opts, to_pf_solution};
pub use surge_network::{AngleReference, DistributedAngleWeight};
pub use types::{
    DcAnalysisRequest, DcAnalysisResult, DcError, DcPfOptions, DcPfSolution, LodfMatrixResult,
    LodfPairs, LodfResult, N2LodfBatchResult, N2LodfResult, OtdfResult, PtdfRows,
};
