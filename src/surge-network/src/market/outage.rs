// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Outage schedule types.

use serde::{Deserialize, Serialize};

use crate::network::EquipmentRef;

/// Physical equipment categories that can experience outages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EquipmentCategory {
    Generator,
    Branch,
    Load,
    Bess,
    LccHvdcLink,
    VscHvdcLink,
    DcGrid,
    LccConverterTerminal,
    DcBranch,
    FactsDevice,
    SwitchedShunt,
    FixedShunt,
    InductionMachine,
    Breaker,
}

/// Outage type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutageType {
    /// Scheduled maintenance — known dates.
    Planned,
    /// Unexpected failure — return time estimated.
    Forced,
    /// Online but at reduced capacity.
    Derate,
    /// Long-term out of service.
    Mothballed,
}

/// An outage or derate event for a piece of equipment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutageEntry {
    /// Stable equipment reference.
    pub equipment: EquipmentRef,
    /// Start time (hours from beginning of study horizon).
    pub start_hr: f64,
    /// End time (hours). f64::INFINITY = unknown return.
    pub end_hr: f64,
    /// Outage classification.
    pub outage_type: OutageType,
    /// Derate factor [0, 1]. 0 = full outage, 0.8 = 80% of rating.
    pub derate_factor: f64,
    /// Reason / work order.
    pub reason: Option<String>,
}

impl OutageEntry {
    pub fn category(&self) -> EquipmentCategory {
        match &self.equipment {
            EquipmentRef::Generator(_) => EquipmentCategory::Generator,
            EquipmentRef::Branch(_) => EquipmentCategory::Branch,
            EquipmentRef::Load(_) => EquipmentCategory::Load,
            EquipmentRef::Bess(_) => EquipmentCategory::Bess,
            EquipmentRef::LccHvdcLink(_) => EquipmentCategory::LccHvdcLink,
            EquipmentRef::VscHvdcLink(_) => EquipmentCategory::VscHvdcLink,
            EquipmentRef::DcGrid(_) => EquipmentCategory::DcGrid,
            EquipmentRef::LccConverterTerminal(_) => EquipmentCategory::LccConverterTerminal,
            EquipmentRef::DcBranch(_) => EquipmentCategory::DcBranch,
            EquipmentRef::FactsDevice(_) => EquipmentCategory::FactsDevice,
            EquipmentRef::SwitchedShunt(_) => EquipmentCategory::SwitchedShunt,
            EquipmentRef::FixedShunt(_) => EquipmentCategory::FixedShunt,
            EquipmentRef::InductionMachine(_) => EquipmentCategory::InductionMachine,
            EquipmentRef::Breaker(_) => EquipmentCategory::Breaker,
        }
    }
}
