// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge Network — canonical power-system domain model.
//!
//! This crate owns the canonical system model used by importers, topology
//! processing, solvers, and product surfaces. It contains no solver
//! implementations.

pub mod dynamics;
pub mod market;
pub mod network;
pub mod synthetic;
pub mod ybus;

pub use network::{AngleReference, DistributedAngleWeight, Network, NetworkError};
pub use ybus::YBus;
