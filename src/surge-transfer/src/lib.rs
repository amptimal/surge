// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge Transfer — Transfer capability analysis.
//!
//! # Modules
//!
//! - [`atc`] — NERC MOD-029/MOD-030 ATC with reactive margin.
//! - [`ac_atc`] — AC-aware ATC with voltage screening.
//! - [`dfax`] — DC PTDF-based AFC and simultaneous transfer studies.
//! - [`matrices`] — GSF and BLDF matrix surfaces.
//! - [`injection`] — injection-capability screening.
pub mod ac_atc;
pub mod atc;
pub mod dfax;
pub mod error;
pub mod injection;
pub mod matrices;
pub mod multi_transfer;
pub mod study;
#[cfg(test)]
mod test_util;
pub mod types;

pub use ac_atc::compute_ac_atc;
pub use atc::compute_nerc_atc;
pub use dfax::{PreparedTransferModel, compute_afc};
pub use error::TransferError;
pub use multi_transfer::compute_multi_transfer;
pub use study::TransferStudy;
pub use types::{
    AcAtcLimitingConstraint, AcAtcRequest, AcAtcResult, AfcRequest, AfcResult, AtcMargins,
    AtcOptions, Flowgate, MultiTransferRequest, MultiTransferResult, NercAtcLimitCause,
    NercAtcRequest, NercAtcResult, TransferPath,
};
