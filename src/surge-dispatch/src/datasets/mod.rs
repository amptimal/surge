// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Dataset importers that translate published power-system test datasets
//! into [`crate::DispatchRequest`] / [`crate::DispatchProfiles`] inputs.
//!
//! These are file-format-specific bridges that naturally live next to the
//! dispatch types they produce. Pure static-network I/O lives in
//! [`surge_io`]; this module owns the dispatch-request-shaped translations.

pub mod activsg;

pub use activsg::{
    ActivsgCase, ActivsgImportError, ActivsgImportOptions, ActivsgImportReport, ActivsgTimeSeries,
    MissingTimestampPolicy, UnmappedSolarBusPolicy, read_tamu_activsg_time_series,
};
