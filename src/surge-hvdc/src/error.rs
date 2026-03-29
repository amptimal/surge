// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Error types for surge-hvdc.

use thiserror::Error as ThisError;

/// Errors that can occur during HVDC power flow solution.
#[derive(Debug, ThisError)]
pub enum HvdcError {
    /// A converter bus number was not found in the network.
    #[error("HVDC converter bus {0} not found in network")]
    BusNotFound(u32),

    /// The underlying AC power flow solver failed.
    #[error("AC power flow failed during HVDC iteration: {0}")]
    AcPfFailed(String),

    /// The HVDC outer iteration did not converge.
    #[error(
        "HVDC AC-DC iteration did not converge in {iterations} iterations (max delta: {max_delta:.3e})"
    )]
    NotConverged { iterations: u32, max_delta: f64 },

    /// The link list is inconsistent (e.g., from_bus == to_bus).
    #[error("Invalid HVDC link configuration: {0}")]
    InvalidLink(String),

    /// A requested HVDC solver method is not available for the current API path.
    #[error("Unsupported HVDC solver method: {0}")]
    UnsupportedMethod(String),

    /// The requested solver cannot handle the current model/control configuration.
    #[error("Unsupported HVDC configuration: {0}")]
    UnsupportedConfiguration(String),
}
