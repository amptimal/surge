// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use surge_dc::DcError;
use thiserror::Error;

/// Canonical error type for transfer studies.
#[derive(Debug, Error)]
pub enum TransferError {
    #[error(transparent)]
    Dc(#[from] DcError),

    #[error("invalid transfer path '{name}': {reason}")]
    InvalidTransferPath { name: String, reason: String },

    #[error("invalid flowgate '{name}': {reason}")]
    InvalidFlowgate { name: String, reason: String },

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("AC power flow failed: {0}")]
    AcPowerFlow(String),

    #[error("solver error: {0}")]
    Solver(String),
}
