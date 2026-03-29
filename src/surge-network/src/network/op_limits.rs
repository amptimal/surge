// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEC 61970-302 Operational Limits — CIM-aligned limit model.
//!
//! This module provides the full operational-limits hierarchy from the CIM
//! OperationalLimits package. It complements the existing `Branch.rating_a_mva/b/c`
//! and `Bus.voltage_min_pu/vmax` fields (which remain the primary inputs for solvers)
//! by preserving the complete limit metadata: duration categories (PATL/TATL/IATL),
//! direction, limit kinds (MW/MVA/A/kV), and CIM traceability via mRIDs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Duration category for operational limits (IEC 61970-302).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LimitDuration {
    /// Permanent Admissible Transmission Loading (PATL) — normal continuous rating.
    Permanent,
    /// Temporary Admissible (TATL) — time-limited overload, duration in seconds.
    Temporary(f64),
    /// Instantaneous Admissible (IATL) — very short duration (typically 0 s).
    Instantaneous,
}

/// Direction of an operational limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LimitDirection {
    High,
    Low,
    AbsoluteValue,
}

/// A single operational limit value with its classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalLimit {
    /// Limit value in engineering units (MW, MVA, A, or kV).
    pub value: f64,
    /// Duration category.
    pub duration: LimitDuration,
    /// Direction.
    pub direction: LimitDirection,
    /// CIM `OperationalLimitType` mRID for traceability.
    pub limit_type_mrid: Option<String>,
}

/// What physical quantity a limit constrains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LimitKind {
    /// Active power (MW).
    ActivePower,
    /// Apparent power (MVA).
    ApparentPower,
    /// Current (A).
    Current,
    /// Voltage (kV).
    Voltage,
}

/// Complete set of operational limits for one terminal/equipment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperationalLimitSet {
    /// CIM mRID.
    pub mrid: String,
    /// Human-readable name.
    pub name: String,
    /// Internal bus number where limits apply (0 if unresolved).
    pub bus: u32,
    /// Equipment mRID (branch circuit or generator).
    pub equipment_mrid: Option<String>,
    /// Whether this is from-end (`true`) or to-end (`false`) of a branch.
    pub from_end: Option<bool>,
    /// All limits in this set, grouped by kind.
    pub limits: Vec<(LimitKind, OperationalLimit)>,
}

/// Container for all operational limits on the Network.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperationalLimits {
    /// All limit sets, keyed by CIM mRID.
    pub limit_sets: HashMap<String, OperationalLimitSet>,
}

impl OperationalLimits {
    /// Returns `true` when no limit sets have been populated.
    pub fn is_empty(&self) -> bool {
        self.limit_sets.is_empty()
    }

    /// Total number of individual limit values across all sets.
    pub fn total_limit_count(&self) -> usize {
        self.limit_sets.values().map(|s| s.limits.len()).sum()
    }

    /// Iterate all limit sets attached to a given equipment mRID.
    pub fn sets_for_equipment<'a>(
        &'a self,
        equipment_mrid: &'a str,
    ) -> impl Iterator<Item = &'a OperationalLimitSet> {
        self.limit_sets.values().filter(move |s| {
            s.equipment_mrid
                .as_deref()
                .map(|e| e == equipment_mrid)
                .unwrap_or(false)
        })
    }

    /// Iterate all limit sets attached to a given bus number.
    pub fn sets_for_bus(&self, bus: u32) -> impl Iterator<Item = &OperationalLimitSet> {
        self.limit_sets.values().filter(move |s| s.bus == bus)
    }
}
