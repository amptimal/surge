// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Python-facing topology view and read-only topology data objects.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::exceptions::{MissingTopologyError, StaleTopologyError, to_topology_pyerr};
use crate::network::Network;
use surge_network::network::topology::TopologyMapping as CoreTopologyMapping;
use surge_network::network::{
    self as core_network, TopologyMappingState as CoreTopologyMappingState,
};

fn switch_kind_name(kind: core_network::SwitchType) -> &'static str {
    match kind {
        core_network::SwitchType::Breaker => "breaker",
        core_network::SwitchType::Disconnector => "disconnector",
        core_network::SwitchType::LoadBreakSwitch => "load_break_switch",
        core_network::SwitchType::Fuse => "fuse",
        core_network::SwitchType::GroundDisconnector => "ground_disconnector",
        core_network::SwitchType::Switch => "switch",
    }
}

fn status_name(status: CoreTopologyMappingState) -> &'static str {
    match status {
        CoreTopologyMappingState::Missing => "missing",
        CoreTopologyMappingState::Current => "current",
        CoreTopologyMappingState::Stale => "stale",
    }
}

fn topology_from_network(network: &Network) -> PyResult<&core_network::NodeBreakerTopology> {
    network
        .inner
        .topology
        .as_ref()
        .ok_or_else(|| MissingTopologyError::new_err("network has no node-breaker topology"))
}

fn wrap_network(parent: &Network, inner: surge_network::Network) -> Network {
    Network {
        inner: Arc::new(inner),
        oltc_controls: parent.oltc_controls.clone(),
        switched_shunts: parent.switched_shunts.clone(),
    }
}

#[pyclass(name = "NodeBreakerTopology", unsendable, skip_from_py_object)]
pub struct NodeBreakerTopologyView {
    pub(crate) parent: Py<Network>,
}

#[pymethods]
impl NodeBreakerTopologyView {
    #[getter]
    fn status(&self, py: Python<'_>) -> PyResult<&'static str> {
        let parent = self.parent.bind(py).borrow();
        Ok(status_name(topology_from_network(&parent)?.status()))
    }

    #[getter]
    fn is_current(&self, py: Python<'_>) -> PyResult<bool> {
        let parent = self.parent.bind(py).borrow();
        Ok(topology_from_network(&parent)?.is_current())
    }

    #[getter]
    fn substations(&self, py: Python<'_>) -> PyResult<Vec<Substation>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .substations
            .iter()
            .cloned()
            .map(|inner| Substation { inner })
            .collect())
    }

    #[getter]
    fn voltage_levels(&self, py: Python<'_>) -> PyResult<Vec<VoltageLevel>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .voltage_levels
            .iter()
            .cloned()
            .map(|inner| VoltageLevel { inner })
            .collect())
    }

    #[getter]
    fn bays(&self, py: Python<'_>) -> PyResult<Vec<Bay>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .bays
            .iter()
            .cloned()
            .map(|inner| Bay { inner })
            .collect())
    }

    #[getter]
    fn connectivity_nodes(&self, py: Python<'_>) -> PyResult<Vec<ConnectivityNode>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .connectivity_nodes
            .iter()
            .cloned()
            .map(|inner| ConnectivityNode { inner })
            .collect())
    }

    #[getter]
    fn busbar_sections(&self, py: Python<'_>) -> PyResult<Vec<BusbarSection>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .busbar_sections
            .iter()
            .cloned()
            .map(|inner| BusbarSection { inner })
            .collect())
    }

    #[getter]
    fn switches(&self, py: Python<'_>) -> PyResult<Vec<TopologySwitch>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .switches
            .iter()
            .cloned()
            .map(|inner| TopologySwitch { inner })
            .collect())
    }

    #[getter]
    fn terminal_connections(&self, py: Python<'_>) -> PyResult<Vec<TerminalConnection>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .terminal_connections
            .iter()
            .cloned()
            .map(|inner| TerminalConnection { inner })
            .collect())
    }

    #[getter]
    fn mapping(&self, py: Python<'_>) -> PyResult<Option<TopologyMapping>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .current_mapping()
            .cloned()
            .map(|inner| TopologyMapping { inner }))
    }

    fn current_mapping(&self, py: Python<'_>) -> PyResult<TopologyMapping> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        match topology.status() {
            CoreTopologyMappingState::Current => Ok(TopologyMapping {
                inner: topology.current_mapping().expect("current mapping").clone(),
            }),
            CoreTopologyMappingState::Stale => Err(StaleTopologyError::new_err(
                "node-breaker topology is stale; call topology.rebuild() first",
            )),
            CoreTopologyMappingState::Missing => Err(MissingTopologyError::new_err(
                "network has no current topology mapping",
            )),
        }
    }

    fn switch(&self, py: Python<'_>, switch_id: &str) -> PyResult<Option<TopologySwitch>> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(topology
            .switches
            .iter()
            .find(|sw| sw.id == switch_id)
            .cloned()
            .map(|inner| TopologySwitch { inner }))
    }

    fn switch_state(&self, py: Python<'_>, switch_id: &str) -> PyResult<Option<bool>> {
        let parent = self.parent.bind(py).borrow();
        Ok(topology_from_network(&parent)?.switch_state(switch_id))
    }

    #[pyo3(signature = (switch_id, *, is_open))]
    fn set_switch_state(&self, py: Python<'_>, switch_id: &str, is_open: bool) -> PyResult<bool> {
        let mut parent = self.parent.bind(py).borrow_mut();
        let topology = std::sync::Arc::make_mut(&mut parent.inner)
            .topology
            .as_mut()
            .ok_or_else(|| MissingTopologyError::new_err("network has no node-breaker topology"))?;
        Ok(topology.set_switch_state(switch_id, is_open))
    }

    fn rebuild(&self, py: Python<'_>) -> PyResult<Network> {
        let parent = self.parent.bind(py).borrow();
        let new_inner =
            surge_topology::rebuild_topology(&parent.inner).map_err(|e| to_topology_pyerr(&e))?;
        Ok(wrap_network(&parent, new_inner))
    }

    fn rebuild_with_report(&self, py: Python<'_>) -> PyResult<TopologyRebuildResult> {
        let parent = self.parent.bind(py).borrow();
        let rebuilt = surge_topology::rebuild_topology_with_report(&parent.inner)
            .map_err(|e| to_topology_pyerr(&e))?;
        Ok(TopologyRebuildResult {
            network: wrap_network(&parent, rebuilt.network),
            report: TopologyReport::from_core(rebuilt.report),
        })
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let parent = self.parent.bind(py).borrow();
        let topology = topology_from_network(&parent)?;
        Ok(format!(
            "NodeBreakerTopology(status='{}', switches={}, connectivity_nodes={})",
            status_name(topology.status()),
            topology.switches.len(),
            topology.connectivity_nodes.len(),
        ))
    }
}

#[pyclass(name = "TopologyMapping", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologyMapping {
    inner: CoreTopologyMapping,
}

#[pymethods]
impl TopologyMapping {
    #[getter]
    fn connectivity_node_to_bus<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (node_id, bus) in &self.inner.connectivity_node_to_bus {
            dict.set_item(node_id, bus)?;
        }
        Ok(dict)
    }

    #[getter]
    fn bus_to_connectivity_nodes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (bus, node_ids) in &self.inner.bus_to_connectivity_nodes {
            dict.set_item(bus, node_ids)?;
        }
        Ok(dict)
    }

    #[getter]
    fn consumed_switch_ids(&self) -> Vec<String> {
        self.inner.consumed_switch_ids.clone()
    }

    #[getter]
    fn isolated_connectivity_node_ids(&self) -> Vec<String> {
        self.inner.isolated_connectivity_node_ids.clone()
    }

    fn bus_for_connectivity_node(&self, connectivity_node_id: &str) -> Option<u32> {
        self.inner
            .connectivity_node_to_bus
            .get(connectivity_node_id)
            .copied()
    }

    fn connectivity_nodes_for_bus(&self, bus_number: u32) -> Option<Vec<String>> {
        self.inner
            .bus_to_connectivity_nodes
            .get(&bus_number)
            .cloned()
    }

    fn __repr__(&self) -> String {
        format!(
            "TopologyMapping(connectivity_nodes={}, buses={})",
            self.inner.connectivity_node_to_bus.len(),
            self.inner.bus_to_connectivity_nodes.len(),
        )
    }
}

#[pyclass(name = "TopologyBusSplit", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologyBusSplit {
    inner: surge_topology::TopologyBusSplit,
}

impl TopologyBusSplit {
    fn from_core(inner: surge_topology::TopologyBusSplit) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl TopologyBusSplit {
    #[getter]
    fn previous_bus_number(&self) -> u32 {
        self.inner.previous_bus_number
    }

    #[getter]
    fn current_bus_numbers(&self) -> Vec<u32> {
        self.inner.current_bus_numbers.clone()
    }
}

#[pyclass(name = "TopologyBusMerge", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologyBusMerge {
    inner: surge_topology::TopologyBusMerge,
}

impl TopologyBusMerge {
    fn from_core(inner: surge_topology::TopologyBusMerge) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl TopologyBusMerge {
    #[getter]
    fn current_bus_number(&self) -> u32 {
        self.inner.current_bus_number
    }

    #[getter]
    fn previous_bus_numbers(&self) -> Vec<u32> {
        self.inner.previous_bus_numbers.clone()
    }
}

#[pyclass(name = "CollapsedBranch", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct CollapsedBranch {
    inner: surge_topology::CollapsedBranch,
}

impl CollapsedBranch {
    fn from_core(inner: surge_topology::CollapsedBranch) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl CollapsedBranch {
    #[getter]
    fn previous_from_bus(&self) -> u32 {
        self.inner.previous_from_bus
    }

    #[getter]
    fn previous_to_bus(&self) -> u32 {
        self.inner.previous_to_bus
    }

    #[getter]
    fn circuit(&self) -> &str {
        &self.inner.circuit
    }
}

#[pyclass(name = "TopologyReport", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologyReport {
    inner: surge_topology::TopologyReport,
}

impl TopologyReport {
    fn from_core(inner: surge_topology::TopologyReport) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl TopologyReport {
    #[getter]
    fn previous_bus_count(&self) -> usize {
        self.inner.previous_bus_count
    }

    #[getter]
    fn current_bus_count(&self) -> usize {
        self.inner.current_bus_count
    }

    #[getter]
    fn bus_splits(&self) -> Vec<TopologyBusSplit> {
        self.inner
            .bus_splits
            .iter()
            .cloned()
            .map(TopologyBusSplit::from_core)
            .collect()
    }

    #[getter]
    fn bus_merges(&self) -> Vec<TopologyBusMerge> {
        self.inner
            .bus_merges
            .iter()
            .cloned()
            .map(TopologyBusMerge::from_core)
            .collect()
    }

    #[getter]
    fn collapsed_branches(&self) -> Vec<CollapsedBranch> {
        self.inner
            .collapsed_branches
            .iter()
            .cloned()
            .map(CollapsedBranch::from_core)
            .collect()
    }

    #[getter]
    fn consumed_switch_ids(&self) -> Vec<String> {
        self.inner.consumed_switch_ids.clone()
    }

    #[getter]
    fn isolated_connectivity_node_ids(&self) -> Vec<String> {
        self.inner.isolated_connectivity_node_ids.clone()
    }
}

#[pyclass(name = "TopologyRebuildResult", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologyRebuildResult {
    network: Network,
    report: TopologyReport,
}

#[pymethods]
impl TopologyRebuildResult {
    #[getter]
    fn network(&self) -> Network {
        self.network.clone()
    }

    #[getter]
    fn report(&self) -> TopologyReport {
        self.report.clone()
    }
}

#[pyclass(name = "Substation", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct Substation {
    inner: core_network::topology::Substation,
}

#[pymethods]
impl Substation {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn region(&self) -> Option<String> {
        self.inner.region.clone()
    }
}

#[pyclass(name = "VoltageLevel", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct VoltageLevel {
    inner: core_network::topology::VoltageLevel,
}

#[pymethods]
impl VoltageLevel {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn substation_id(&self) -> &str {
        &self.inner.substation_id
    }

    #[getter]
    fn base_kv(&self) -> f64 {
        self.inner.base_kv
    }
}

#[pyclass(name = "Bay", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct Bay {
    inner: core_network::topology::Bay,
}

#[pymethods]
impl Bay {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn voltage_level_id(&self) -> &str {
        &self.inner.voltage_level_id
    }
}

#[pyclass(name = "ConnectivityNode", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct ConnectivityNode {
    inner: core_network::topology::ConnectivityNode,
}

#[pymethods]
impl ConnectivityNode {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn voltage_level_id(&self) -> &str {
        &self.inner.voltage_level_id
    }
}

#[pyclass(name = "BusbarSection", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct BusbarSection {
    inner: core_network::topology::BusbarSection,
}

#[pymethods]
impl BusbarSection {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn connectivity_node_id(&self) -> &str {
        &self.inner.connectivity_node_id
    }

    #[getter]
    fn ip_max(&self) -> Option<f64> {
        self.inner.ip_max
    }
}

#[pyclass(name = "TerminalConnection", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TerminalConnection {
    inner: core_network::topology::TerminalConnection,
}

#[pymethods]
impl TerminalConnection {
    #[getter]
    fn terminal_id(&self) -> &str {
        &self.inner.terminal_id
    }

    #[getter]
    fn equipment_id(&self) -> &str {
        &self.inner.equipment_id
    }

    #[getter]
    fn equipment_class(&self) -> &str {
        &self.inner.equipment_class
    }

    #[getter]
    fn sequence_number(&self) -> u32 {
        self.inner.sequence_number
    }

    #[getter]
    fn connectivity_node_id(&self) -> &str {
        &self.inner.connectivity_node_id
    }
}

#[pyclass(name = "TopologySwitch", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct TopologySwitch {
    inner: core_network::SwitchDevice,
}

#[pymethods]
impl TopologySwitch {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn kind(&self) -> &'static str {
        switch_kind_name(self.inner.switch_type)
    }

    #[getter]
    fn is_open(&self) -> bool {
        self.inner.open
    }

    #[getter]
    fn normally_open(&self) -> bool {
        self.inner.normal_open
    }

    #[getter]
    fn retained(&self) -> bool {
        self.inner.retained
    }

    #[getter]
    fn rated_current_amp(&self) -> Option<f64> {
        self.inner.rated_current
    }

    #[getter]
    fn from_connectivity_node_id(&self) -> &str {
        &self.inner.cn1_id
    }

    #[getter]
    fn to_connectivity_node_id(&self) -> &str {
        &self.inner.cn2_id
    }

    fn __repr__(&self) -> String {
        format!(
            "TopologySwitch(id='{}', kind='{}', is_open={})",
            self.inner.id,
            switch_kind_name(self.inner.switch_type),
            self.inner.open,
        )
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NodeBreakerTopologyView>()?;
    m.add_class::<TopologyMapping>()?;
    m.add_class::<TopologyBusSplit>()?;
    m.add_class::<TopologyBusMerge>()?;
    m.add_class::<CollapsedBranch>()?;
    m.add_class::<TopologyReport>()?;
    m.add_class::<TopologyRebuildResult>()?;
    m.add_class::<Substation>()?;
    m.add_class::<VoltageLevel>()?;
    m.add_class::<Bay>()?;
    m.add_class::<ConnectivityNode>()?;
    m.add_class::<BusbarSection>()?;
    m.add_class::<TerminalConnection>()?;
    m.add_class::<TopologySwitch>()?;
    Ok(())
}
