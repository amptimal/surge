// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Stable public equipment references used by the canonical network model.

use serde::{Deserialize, Serialize};

use crate::network::Branch;

/// Stable directional branch identity.
///
/// Unlike [`crate::network::BranchEquipmentKey`], this preserves authored
/// direction so interfaces, flowgates, and equipment refs can express a signed
/// flow convention directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchRef {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
}

impl BranchRef {
    pub fn new(from_bus: u32, to_bus: u32, circuit: impl Into<String>) -> Self {
        Self {
            from_bus,
            to_bus,
            circuit: circuit.into(),
        }
    }

    pub fn matches_branch(&self, branch: &Branch) -> bool {
        branch.from_bus == self.from_bus
            && branch.to_bus == self.to_bus
            && branch.circuit == self.circuit
    }
}

impl From<(u32, u32, String)> for BranchRef {
    fn from(value: (u32, u32, String)) -> Self {
        Self::new(value.0, value.1, value.2)
    }
}

impl From<&(u32, u32, String)> for BranchRef {
    fn from(value: &(u32, u32, String)) -> Self {
        Self::new(value.0, value.1, value.2.clone())
    }
}

impl From<&Branch> for BranchRef {
    fn from(value: &Branch) -> Self {
        Self::new(value.from_bus, value.to_bus, value.circuit.clone())
    }
}

impl From<BranchRef> for (u32, u32, String) {
    fn from(value: BranchRef) -> Self {
        (value.from_bus, value.to_bus, value.circuit)
    }
}

/// A directed branch reference with an associated scalar coefficient.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedBranchRef {
    pub branch: BranchRef,
    pub coefficient: f64,
}

impl WeightedBranchRef {
    pub fn new(from_bus: u32, to_bus: u32, circuit: impl Into<String>, coefficient: f64) -> Self {
        Self {
            branch: BranchRef::new(from_bus, to_bus, circuit),
            coefficient,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GeneratorRef {
    pub bus: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LoadRef {
    pub bus: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FixedShuntRef {
    pub bus: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SwitchedShuntRef {
    pub bus: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FactsDeviceRef {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HvdcLinkRef {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DcGridRef {
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DcConverterRef {
    pub grid_id: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DcBranchRef {
    pub grid_id: u32,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LccTerminalSide {
    Rectifier,
    Inverter,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LccConverterTerminalRef {
    pub link_name: String,
    pub terminal: LccTerminalSide,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InductionMachineRef {
    pub bus: u32,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BreakerRef {
    pub bus: u32,
    pub name: String,
}

/// Stable public reference to any outage-addressable equipment in the network.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EquipmentRef {
    Generator(GeneratorRef),
    Branch(BranchRef),
    Load(LoadRef),
    Bess(GeneratorRef),
    LccHvdcLink(HvdcLinkRef),
    VscHvdcLink(HvdcLinkRef),
    DcGrid(DcGridRef),
    LccConverterTerminal(LccConverterTerminalRef),
    DcBranch(DcBranchRef),
    FactsDevice(FactsDeviceRef),
    SwitchedShunt(SwitchedShuntRef),
    FixedShunt(FixedShuntRef),
    InductionMachine(InductionMachineRef),
    Breaker(BreakerRef),
}
