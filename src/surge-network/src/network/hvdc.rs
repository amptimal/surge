// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Canonical HVDC domain model.
//!
//! This module is the only public HVDC surface for [`crate::network::Network`].
//! Source-format specific records stay out of the canonical network model.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use crate::network::dc_line::{LccConverterTerminal, LccHvdcControlMode, LccHvdcLink};
pub use crate::network::dc_network_types::{DcBranch, DcBus, DcConverterStation};
pub use crate::network::vsc_dc_line::{
    VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
};

/// A point-to-point HVDC link.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "technology", rename_all = "snake_case")]
pub enum HvdcLink {
    Lcc(LccHvdcLink),
    Vsc(VscHvdcLink),
}

impl HvdcLink {
    /// Stable user-facing name for the link.
    pub fn name(&self) -> &str {
        match self {
            Self::Lcc(link) => &link.name,
            Self::Vsc(link) => &link.name,
        }
    }

    /// Whether the link is blocked / out of service.
    pub fn is_blocked(&self) -> bool {
        match self {
            Self::Lcc(link) => link.mode == LccHvdcControlMode::Blocked,
            Self::Vsc(link) => link.mode == VscHvdcControlMode::Blocked,
        }
    }

    pub fn as_lcc(&self) -> Option<&LccHvdcLink> {
        match self {
            Self::Lcc(link) => Some(link),
            Self::Vsc(_) => None,
        }
    }

    pub fn as_lcc_mut(&mut self) -> Option<&mut LccHvdcLink> {
        match self {
            Self::Lcc(link) => Some(link),
            Self::Vsc(_) => None,
        }
    }

    pub fn as_vsc(&self) -> Option<&VscHvdcLink> {
        match self {
            Self::Lcc(_) => None,
            Self::Vsc(link) => Some(link),
        }
    }

    pub fn as_vsc_mut(&mut self) -> Option<&mut VscHvdcLink> {
        match self {
            Self::Lcc(_) => None,
            Self::Vsc(link) => Some(link),
        }
    }
}

/// Converter role within an LCC DC grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LccDcConverterRole {
    #[default]
    Rectifier,
    Inverter,
}

/// Canonical LCC converter record for explicit DC-grid topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LccDcConverter {
    /// Stable converter identifier within the enclosing DC grid.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// DC bus number this converter connects to.
    pub dc_bus: u32,
    /// AC bus number this converter connects to.
    pub ac_bus: u32,
    /// Number of 6-pulse bridges.
    #[serde(alias = "num_bridges")]
    pub n_bridges: u32,
    /// Maximum firing / extinction angle in degrees.
    pub alpha_max_deg: f64,
    /// Minimum firing / extinction angle in degrees.
    pub alpha_min_deg: f64,
    /// Minimum extinction angle in inverter mode.
    pub gamma_min_deg: f64,
    /// Commutating resistance per bridge in ohms.
    pub commutation_resistance_ohm: f64,
    /// Commutating reactance per bridge in ohms.
    pub commutation_reactance_ohm: f64,
    /// Converter transformer rated AC voltage on the network side.
    pub base_voltage_kv: f64,
    /// Transformer turns ratio.
    pub turns_ratio: f64,
    /// Off-nominal tap ratio.
    pub tap_ratio: f64,
    /// Maximum tap ratio.
    pub tap_max: f64,
    /// Minimum tap ratio.
    pub tap_min: f64,
    /// Tap step size.
    pub tap_step: f64,
    /// Scheduled power (MW) or current (kA) setpoint.
    pub scheduled_setpoint: f64,
    /// Share of total DC power assigned to this converter.
    pub power_share_percent: f64,
    /// Current margin percentage.
    pub current_margin_percent: f64,
    /// Rectifier or inverter role.
    pub role: LccDcConverterRole,
    /// Converter in-service flag.
    pub in_service: bool,
}

impl Default for LccDcConverter {
    fn default() -> Self {
        Self {
            id: String::new(),
            dc_bus: 0,
            ac_bus: 0,
            n_bridges: 1,
            alpha_max_deg: 90.0,
            alpha_min_deg: 5.0,
            gamma_min_deg: 15.0,
            commutation_resistance_ohm: 0.0,
            commutation_reactance_ohm: 0.0,
            base_voltage_kv: 0.0,
            turns_ratio: 1.0,
            tap_ratio: 1.0,
            tap_max: 1.1,
            tap_min: 0.9,
            tap_step: 0.00625,
            scheduled_setpoint: 0.0,
            power_share_percent: 0.0,
            current_margin_percent: 0.0,
            role: LccDcConverterRole::Rectifier,
            in_service: true,
        }
    }
}

/// Canonical DC-grid converter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "technology", rename_all = "snake_case")]
pub enum DcConverter {
    Lcc(LccDcConverter),
    Vsc(DcConverterStation),
}

impl From<LccDcConverter> for DcConverter {
    fn from(value: LccDcConverter) -> Self {
        Self::Lcc(value)
    }
}

impl From<DcConverterStation> for DcConverter {
    fn from(value: DcConverterStation) -> Self {
        Self::Vsc(value)
    }
}

impl DcConverter {
    pub fn id(&self) -> &str {
        match self {
            Self::Lcc(converter) => &converter.id,
            Self::Vsc(converter) => &converter.id,
        }
    }

    pub fn id_mut(&mut self) -> &mut String {
        match self {
            Self::Lcc(converter) => &mut converter.id,
            Self::Vsc(converter) => &mut converter.id,
        }
    }

    pub fn ac_bus(&self) -> u32 {
        match self {
            Self::Lcc(converter) => converter.ac_bus,
            Self::Vsc(converter) => converter.ac_bus,
        }
    }

    pub fn dc_bus(&self) -> u32 {
        match self {
            Self::Lcc(converter) => converter.dc_bus,
            Self::Vsc(converter) => converter.dc_bus,
        }
    }

    pub fn is_in_service(&self) -> bool {
        match self {
            Self::Lcc(converter) => converter.in_service,
            Self::Vsc(converter) => converter.status,
        }
    }

    pub fn is_lcc(&self) -> bool {
        matches!(self, Self::Lcc(_))
    }

    pub fn as_lcc(&self) -> Option<&LccDcConverter> {
        match self {
            Self::Lcc(converter) => Some(converter),
            Self::Vsc(_) => None,
        }
    }

    pub fn as_lcc_mut(&mut self) -> Option<&mut LccDcConverter> {
        match self {
            Self::Lcc(converter) => Some(converter),
            Self::Vsc(_) => None,
        }
    }

    pub fn as_vsc(&self) -> Option<&DcConverterStation> {
        match self {
            Self::Lcc(_) => None,
            Self::Vsc(converter) => Some(converter),
        }
    }

    pub fn as_vsc_mut(&mut self) -> Option<&mut DcConverterStation> {
        match self {
            Self::Lcc(_) => None,
            Self::Vsc(converter) => Some(converter),
        }
    }

    pub fn ac_bus_mut(&mut self) -> &mut u32 {
        match self {
            Self::Lcc(converter) => &mut converter.ac_bus,
            Self::Vsc(converter) => &mut converter.ac_bus,
        }
    }

    pub fn dc_bus_mut(&mut self) -> &mut u32 {
        match self {
            Self::Lcc(converter) => &mut converter.dc_bus,
            Self::Vsc(converter) => &mut converter.dc_bus,
        }
    }
}

/// Explicit DC-grid topology.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DcGrid {
    /// Stable grid identifier within the network.
    pub id: u32,
    /// Optional user-facing name when the source format provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// DC buses in this grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<DcBus>,
    /// DC-grid converters in this grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub converters: Vec<DcConverter>,
    /// DC branches in this grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<DcBranch>,
}

impl DcGrid {
    pub fn new(id: u32, name: Option<String>) -> Self {
        Self {
            id,
            name,
            buses: Vec::new(),
            converters: Vec::new(),
            branches: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buses.is_empty() && self.converters.is_empty() && self.branches.is_empty()
    }

    pub fn find_bus(&self, bus_id: u32) -> Option<&DcBus> {
        self.buses.iter().find(|bus| bus.bus_id == bus_id)
    }

    pub fn find_bus_mut(&mut self, bus_id: u32) -> Option<&mut DcBus> {
        self.buses.iter_mut().find(|bus| bus.bus_id == bus_id)
    }

    pub fn bus_index_map(&self) -> HashMap<u32, usize> {
        self.buses
            .iter()
            .enumerate()
            .map(|(index, bus)| (bus.bus_id, index))
            .collect()
    }

    pub fn canonicalize_converter_ids(&mut self) {
        for (index, converter) in self.converters.iter_mut().enumerate() {
            let trimmed = converter.id().trim().to_string();
            if trimmed.is_empty() {
                *converter.id_mut() = format!("dc_grid_{}_converter_{}", self.id, index + 1);
            } else if trimmed != converter.id() {
                *converter.id_mut() = trimmed;
            }
        }
    }

    pub fn canonicalize_branch_ids(&mut self) {
        for (index, branch) in self.branches.iter_mut().enumerate() {
            let trimmed = branch.id.trim().to_string();
            if trimmed.is_empty() {
                branch.id = format!("dc_grid_{}_branch_{}", self.id, index + 1);
            } else if trimmed != branch.id {
                branch.id = trimmed;
            }
        }
    }
}

/// Canonical HVDC namespace on [`crate::network::Network`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HvdcModel {
    /// Point-to-point HVDC links.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<HvdcLink>,
    /// Explicit DC grids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dc_grids: Vec<DcGrid>,
}

impl HvdcModel {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.dc_grids.iter().all(DcGrid::is_empty)
    }

    pub fn has_point_to_point_links(&self) -> bool {
        !self.links.is_empty()
    }

    pub fn has_explicit_dc_topology(&self) -> bool {
        self.dc_grids.iter().any(|grid| !grid.is_empty())
    }

    pub fn push_link(&mut self, link: HvdcLink) {
        self.links.push(link);
    }

    pub fn push_lcc_link(&mut self, link: LccHvdcLink) {
        self.links.push(HvdcLink::Lcc(link));
    }

    pub fn push_vsc_link(&mut self, link: VscHvdcLink) {
        self.links.push(HvdcLink::Vsc(link));
    }

    pub fn ensure_dc_grid(&mut self, id: u32, name: Option<String>) -> &mut DcGrid {
        if let Some(index) = self.dc_grids.iter().position(|grid| grid.id == id) {
            let grid = &mut self.dc_grids[index];
            if grid.name.is_none() {
                grid.name = name;
            }
            return grid;
        }
        self.dc_grids.push(DcGrid::new(id, name));
        self.dc_grids.last_mut().expect("grid just inserted")
    }

    pub fn find_dc_grid(&self, id: u32) -> Option<&DcGrid> {
        self.dc_grids.iter().find(|grid| grid.id == id)
    }

    pub fn find_dc_grid_mut(&mut self, id: u32) -> Option<&mut DcGrid> {
        self.dc_grids.iter_mut().find(|grid| grid.id == id)
    }

    pub fn find_dc_grid_by_bus(&self, bus_id: u32) -> Option<&DcGrid> {
        self.dc_grids
            .iter()
            .find(|grid| grid.buses.iter().any(|bus| bus.bus_id == bus_id))
    }

    pub fn find_dc_grid_by_bus_mut(&mut self, bus_id: u32) -> Option<&mut DcGrid> {
        self.dc_grids
            .iter_mut()
            .find(|grid| grid.buses.iter().any(|bus| bus.bus_id == bus_id))
    }

    pub fn find_dc_bus(&self, bus_id: u32) -> Option<&DcBus> {
        self.dc_grids.iter().find_map(|grid| grid.find_bus(bus_id))
    }

    pub fn find_dc_bus_mut(&mut self, bus_id: u32) -> Option<&mut DcBus> {
        self.dc_grids
            .iter_mut()
            .find_map(|grid| grid.find_bus_mut(bus_id))
    }

    pub fn dc_bus_count(&self) -> usize {
        self.dc_grids.iter().map(|grid| grid.buses.len()).sum()
    }

    pub fn dc_converter_count(&self) -> usize {
        self.dc_grids.iter().map(|grid| grid.converters.len()).sum()
    }

    pub fn dc_branch_count(&self) -> usize {
        self.dc_grids.iter().map(|grid| grid.branches.len()).sum()
    }

    pub fn next_dc_grid_id(&self) -> u32 {
        self.dc_grids.iter().map(|grid| grid.id).max().unwrap_or(0) + 1
    }

    pub fn next_dc_bus_id(&self) -> u32 {
        self.dc_grids
            .iter()
            .flat_map(|grid| grid.buses.iter().map(|bus| bus.bus_id))
            .max()
            .unwrap_or(0)
            + 1
    }

    pub fn clear_dc_grids(&mut self) {
        self.dc_grids.clear();
    }

    pub fn canonicalize_converter_ids(&mut self) {
        for grid in &mut self.dc_grids {
            grid.canonicalize_converter_ids();
            grid.canonicalize_branch_ids();
        }
    }

    pub fn dc_buses(&self) -> impl Iterator<Item = &DcBus> {
        self.dc_grids.iter().flat_map(|grid| grid.buses.iter())
    }

    pub fn dc_buses_mut(&mut self) -> impl Iterator<Item = &mut DcBus> {
        self.dc_grids
            .iter_mut()
            .flat_map(|grid| grid.buses.iter_mut())
    }

    pub fn dc_converters(&self) -> impl Iterator<Item = &DcConverter> {
        self.dc_grids.iter().flat_map(|grid| grid.converters.iter())
    }

    pub fn dc_converters_mut(&mut self) -> impl Iterator<Item = &mut DcConverter> {
        self.dc_grids
            .iter_mut()
            .flat_map(|grid| grid.converters.iter_mut())
    }

    pub fn dc_branches(&self) -> impl Iterator<Item = &DcBranch> {
        self.dc_grids.iter().flat_map(|grid| grid.branches.iter())
    }

    pub fn dc_branches_mut(&mut self) -> impl Iterator<Item = &mut DcBranch> {
        self.dc_grids
            .iter_mut()
            .flat_map(|grid| grid.branches.iter_mut())
    }
}
