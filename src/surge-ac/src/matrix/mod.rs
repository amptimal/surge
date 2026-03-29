// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network matrix construction — Y-bus, Jacobian, fused mismatch+Jacobian, power injections.

pub mod fused;
pub mod jacobian;
pub mod mismatch;
pub mod ybus;

/// Sentinel value for "bus not in this index set".
pub(crate) const SENTINEL: usize = usize::MAX;
