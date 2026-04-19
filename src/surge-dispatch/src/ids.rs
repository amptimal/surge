// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stable public identifier newtypes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable public area identifier.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct AreaId(pub u32);

impl AreaId {
    /// Return this identifier as a vector-friendly index.
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl From<u32> for AreaId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<usize> for AreaId {
    fn from(value: usize) -> Self {
        Self(value as u32)
    }
}

/// Stable public zone identifier.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct ZoneId(pub u32);

impl ZoneId {
    /// Return this identifier as a vector-friendly index.
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl From<u32> for ZoneId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<usize> for ZoneId {
    fn from(value: usize) -> Self {
        Self(value as u32)
    }
}
