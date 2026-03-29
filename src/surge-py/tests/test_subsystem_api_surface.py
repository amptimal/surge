# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

import pytest

import surge


def _generator_id_at_bus(network, bus, machine_id="1"):
    for generator in network.generators:
        if generator.bus == bus and generator.machine_id == machine_id:
            return generator.id
    raise AssertionError(f"generator not found at bus={bus} machine_id={machine_id}")


def _subsystem_network():
    net = surge.Network("subsystem-demo")
    net.add_bus_object(surge.Bus(1, "Slack", 345.0, area=1, zone=10, name="north-slack"))
    net.add_bus_object(surge.Bus(2, "PV", 345.0, area=1, zone=10, name="north-gen"))
    net.add_bus_object(
        surge.Bus(
            3,
            "PQ",
            138.0,
            area=1,
            zone=20,
            name="north-load",
            pd_mw=80.0,
            qd_mvar=20.0,
        )
    )
    net.add_bus_object(
        surge.Bus(
            4,
            "PQ",
            138.0,
            area=2,
            zone=20,
            name="south-load",
            pd_mw=50.0,
            qd_mvar=15.0,
        )
    )

    net.add_branch(1, 2, r=0.01, x=0.08, circuit=1)
    net.add_branch(2, 3, r=0.01, x=0.09, circuit=1)
    net.add_branch(3, 4, r=0.02, x=0.10, circuit=1)
    net.add_branch(1, 4, r=0.03, x=0.11, circuit=2)

    net.add_generator(1, p_mw=90.0, pmax_mw=140.0)
    net.add_generator(2, p_mw=40.0, pmax_mw=80.0, machine_id="G2")
    net.add_generator(4, p_mw=20.0, pmax_mw=40.0)
    net.set_generator_in_service(_generator_id_at_bus(net, 4), False)
    return net


def test_subsystem_filters_are_intersection_based():
    net = _subsystem_network()

    assert surge.subsystem.Subsystem(net, areas=[1]).bus_numbers == [1, 2, 3]
    assert surge.subsystem.Subsystem(net, zones=[20]).bus_numbers == [3, 4]
    assert surge.subsystem.Subsystem(net, kv_min=200.0).bus_numbers == [1, 2]
    assert surge.subsystem.Subsystem(net, kv_max=200.0).bus_numbers == [3, 4]
    assert surge.subsystem.Subsystem(net, buses=[4, 1]).bus_numbers == [1, 4]
    assert surge.subsystem.Subsystem(net, bus_type="pq").bus_numbers == [3, 4]

    combined = surge.subsystem.Subsystem(net, areas=[1], zones=[20], kv_max=200.0)
    assert combined.bus_numbers == [3]


def test_subsystem_exposes_internal_and_tie_elements():
    net = _subsystem_network()
    area_one = surge.subsystem.Subsystem(net, name="area-1", areas=[1])

    assert area_one.branches == [(1, 2, "1"), (2, 3, "1")]
    assert area_one.tie_branches == [(3, 4, "1"), (1, 4, "2")]
    assert area_one.generators == [(1, "1"), (2, "G2")]
    assert area_one.loads == [3]
    assert area_one.total_load_mw == pytest.approx(80.0)
    assert area_one.total_generation_mw == pytest.approx(130.0)


def test_subsystem_reflects_live_network_values_but_fixed_bus_set():
    net = _subsystem_network()
    area_one = surge.subsystem.Subsystem(net, name="area-1", areas=[1])

    net.set_bus_load(3, pd_mw=95.0, qd_mvar=22.0)
    net.set_generator_p(_generator_id_at_bus(net, 2, "G2"), 55.0)
    net.add_bus_object(surge.Bus(5, "PQ", 138.0, area=1, zone=20, name="new"))

    assert area_one.bus_numbers == [1, 2, 3]
    assert area_one.total_load_mw == pytest.approx(95.0)
    assert area_one.total_generation_mw == pytest.approx(145.0)


def test_subsystem_empty_case_and_dunders():
    net = _subsystem_network()
    empty = surge.subsystem.Subsystem(net, name="missing", areas=[99])
    area_one = surge.subsystem.Subsystem(net, name="area-1", areas=[1])

    assert len(empty) == 0
    assert empty.bus_numbers == []
    assert empty.branches == []
    assert empty.tie_branches == []
    assert empty.generators == []
    assert empty.loads == []
    assert empty.total_load_mw == pytest.approx(0.0)
    assert empty.total_generation_mw == pytest.approx(0.0)

    assert 1 in area_one
    assert 4 not in area_one
    assert "area-1" in repr(area_one)
    assert "3 buses" in repr(area_one)
