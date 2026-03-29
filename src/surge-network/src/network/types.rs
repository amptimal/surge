// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Fundamental types used throughout the Surge solver.

/// Complex number type (re + im*j) used for admittances, voltages, currents.
pub type Complex = num_complex::Complex64;

/// System base power in MVA (typically 100).
pub const DEFAULT_BASE_MVA: f64 = 100.0;
