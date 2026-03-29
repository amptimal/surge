# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import pytest

import surge


NB_TOGGLE_XIIDM = """<?xml version="1.0" encoding="UTF-8"?>
<iidm:network xmlns:iidm="http://www.powsybl.org/schema/iidm/1_1"
    id="nb_toggle" caseDate="2024-01-01T00:00:00.000Z" forecastDistance="0"
    sourceFormat="test" minimumValidationLevel="STEADY_STATE_HYPOTHESIS">
  <iidm:substation id="SUB1" name="Station1" country="US">
    <iidm:voltageLevel id="VL1" nominalV="345.0" topologyKind="NODE_BREAKER">
      <iidm:nodeBreakerTopology>
        <iidm:busbarSection id="BBS1" node="0"/>
        <iidm:switch id="BRK1" kind="BREAKER" retained="false" open="false" node1="0" node2="1"/>
        <iidm:busbarSection id="BBS2" node="2"/>
        <iidm:switch id="BRK2" kind="BREAKER" retained="false" open="false" node1="1" node2="2"/>
        <iidm:bus v="345.0" angle="0.0" nodes="0,1,2"/>
      </iidm:nodeBreakerTopology>
      <iidm:generator id="GEN1" node="0" energySource="OTHER"
          minP="0.0" maxP="200.0" voltageRegulatorOn="true" targetP="100.0"
          targetV="345.0" targetQ="0.0"/>
    </iidm:voltageLevel>
  </iidm:substation>
</iidm:network>"""


def load_node_breaker_network():
    return surge.io.loads(NB_TOGGLE_XIIDM, surge.io.Format.XIIDM)


def test_topology_surface_is_canonical_and_typed():
    net = load_node_breaker_network()
    topology = net.topology

    assert topology is not None
    assert topology.status == "current"
    assert topology.is_current is True
    assert not hasattr(net, "list_switches")
    assert not hasattr(net, "set_switch")
    assert not hasattr(net, "get_switch_state")

    switch = topology.switch("BRK1")
    assert switch is not None
    assert switch.kind == "breaker"
    assert switch.is_open is False
    assert switch.from_connectivity_node_id == "VL1_N0"
    assert switch.to_connectivity_node_id == "VL1_N1"
    assert not hasattr(switch, "bus1")
    assert not hasattr(switch, "bus2")

    mapping = topology.current_mapping()
    assert mapping.bus_for_connectivity_node("VL1_N0") == 1
    assert mapping.bus_for_connectivity_node("VL1_N2") == 1
    assert mapping.connectivity_nodes_for_bus(1) == ["VL1_N0", "VL1_N1", "VL1_N2"]


def test_topology_requires_explicit_rebuild_after_switch_change():
    net = load_node_breaker_network()
    topology = net.topology
    assert topology is not None

    assert topology.set_switch_state("BRK1", is_open=True) is True
    assert topology.status == "stale"
    assert topology.is_current is False
    assert topology.mapping is None

    with pytest.raises(surge.StaleTopologyError, match="topology.rebuild"):
        topology.current_mapping()

    with pytest.raises(surge.NetworkError, match="rebuild_topology"):
        net.validate()

    with pytest.raises(surge.NetworkError, match="rebuild_topology"):
        surge.solve_ac_pf(net)

    rebuilt_result = topology.rebuild_with_report()
    rebuilt = rebuilt_result.network
    report = rebuilt_result.report

    assert report.previous_bus_count == 1
    assert report.current_bus_count == 2
    assert len(report.bus_splits) == 1
    assert report.bus_splits[0].previous_bus_number == 1
    assert report.bus_splits[0].current_bus_numbers == [1, 2]
    assert len(report.bus_merges) == 0
    assert len(report.collapsed_branches) == 0
    assert report.consumed_switch_ids == ["BRK2"]
    assert report.isolated_connectivity_node_ids == ["VL1_N1", "VL1_N2"]

    rebuilt_topology = rebuilt.topology
    assert rebuilt_topology is not None
    assert rebuilt_topology.status == "current"
    rebuilt_mapping = rebuilt_topology.current_mapping()
    assert rebuilt_mapping.bus_for_connectivity_node("VL1_N0") == 1
    assert rebuilt_mapping.bus_for_connectivity_node("VL1_N2") == 2
