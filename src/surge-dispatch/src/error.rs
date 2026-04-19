// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dispatch error types.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DispatchError {
    #[error("no slack bus found")]
    NoSlackBus,

    #[error("no in-service generators")]
    NoGenerators,

    #[error("generator {gen_idx} (bus {bus}) has no cost curve")]
    MissingCost { gen_idx: usize, bus: u32 },

    #[error("insufficient capacity: load={load_mw:.1} MW, capacity={capacity_mw:.1} MW")]
    InsufficientCapacity { load_mw: f64, capacity_mw: f64 },

    #[error("insufficient reserve: required={required_mw:.1} MW, available={available_mw:.1} MW")]
    InsufficientReserve { required_mw: f64, available_mw: f64 },

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("solver error: {0}")]
    SolverError(String),

    #[error("solver did not converge after {iterations} iterations")]
    NotConverged { iterations: u32 },

    #[error("hour {hour} out of range (n_hours={n_hours})")]
    HourOutOfRange { hour: usize, n_hours: usize },
}

pub(crate) type ScedError = DispatchError;
