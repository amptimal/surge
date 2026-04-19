# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Tests for the Network.add_storage() / set_generator_storage() bindings."""

from __future__ import annotations

import pytest

import surge
from surge import StorageParams


def _single_bus_network() -> "surge.Network":
    net = surge.Network(base_mva=100.0)
    net.add_bus(number=1, bus_type="Slack", base_kv=138.0)
    return net


def test_add_storage_creates_generator_with_storage() -> None:
    net = _single_bus_network()
    params = StorageParams(
        energy_capacity_mwh=100.0,
        charge_efficiency=0.90,
        discharge_efficiency=0.98,
        soc_initial_mwh=50.0,
        soc_min_mwh=10.0,
        soc_max_mwh=95.0,
    )
    rid = net.add_storage(
        bus=1, charge_mw_max=25.0, discharge_mw_max=25.0, params=params
    )
    assert rid == "gen_1_1"
    gens = list(net.generators)
    assert len(gens) == 1
    g = gens[0]
    assert g.pmin_mw == -25.0
    assert g.pmax_mw == 25.0
    assert g.storage is not None
    assert g.storage.energy_capacity_mwh == 100.0
    assert g.storage.charge_efficiency == pytest.approx(0.90)
    assert g.storage.discharge_efficiency == pytest.approx(0.98)
    assert g.storage.round_trip_efficiency == pytest.approx(0.90 * 0.98)
    assert g.storage.soc_initial_mwh == 50.0


def test_legacy_round_trip_efficiency_splits_sqrt_per_leg() -> None:
    """Back-compat shim: a single ``efficiency=`` arg splits sqrt per leg."""
    params = StorageParams(energy_capacity_mwh=100.0, efficiency=0.64)
    assert params.charge_efficiency == pytest.approx(0.8)
    assert params.discharge_efficiency == pytest.approx(0.8)
    assert params.round_trip_efficiency == pytest.approx(0.64)


def test_add_storage_explicit_id() -> None:
    net = _single_bus_network()
    rid = net.add_storage(
        bus=1,
        charge_mw_max=10.0,
        discharge_mw_max=10.0,
        params=StorageParams(energy_capacity_mwh=40.0),
        id="bess_site_A",
    )
    assert rid == "bess_site_A"


def test_add_storage_rejects_missing_bus() -> None:
    net = _single_bus_network()
    with pytest.raises(Exception, match="Bus 99 not found"):
        net.add_storage(
            bus=99,
            charge_mw_max=10.0,
            discharge_mw_max=10.0,
            params=StorageParams(energy_capacity_mwh=40.0),
        )


def test_add_storage_validates_storage_params() -> None:
    net = _single_bus_network()
    # soc_initial outside [soc_min, soc_max]
    bad = StorageParams(
        energy_capacity_mwh=100.0,
        soc_initial_mwh=500.0,  # too high
        soc_min_mwh=0.0,
        soc_max_mwh=100.0,
    )
    with pytest.raises(ValueError, match="invalid StorageParams"):
        net.add_storage(bus=1, charge_mw_max=25.0, discharge_mw_max=25.0, params=bad)


def test_add_storage_rejects_negative_power() -> None:
    net = _single_bus_network()
    params = StorageParams(energy_capacity_mwh=50.0)
    with pytest.raises(ValueError, match="charge_mw_max"):
        net.add_storage(bus=1, charge_mw_max=-5.0, discharge_mw_max=10.0, params=params)
    with pytest.raises(ValueError, match="discharge_mw_max"):
        net.add_storage(bus=1, charge_mw_max=10.0, discharge_mw_max=-5.0, params=params)


def test_set_and_clear_generator_storage() -> None:
    net = _single_bus_network()
    # Start with a conventional generator.
    rid = net.add_generator(
        bus=1, p_mw=0.0, pmax_mw=50.0, pmin_mw=0.0, vs_pu=1.0,
        qmax_mvar=0.0, qmin_mvar=0.0, machine_id="1",
    )
    g = list(net.generators)[0]
    assert g.storage is None

    net.set_generator_storage(
        rid,
        StorageParams(energy_capacity_mwh=80.0, soc_initial_mwh=40.0),
    )
    g = list(net.generators)[0]
    assert g.storage is not None
    assert g.storage.energy_capacity_mwh == 80.0

    net.clear_generator_storage(rid)
    g = list(net.generators)[0]
    assert g.storage is None


def test_solve_dispatch_with_storage_time_coupled() -> None:
    """Battery should absorb high prices early and release to cheap periods
    — or, with a flat price curve and cheap on-site gen, the LP should at
    least exercise SOC dynamics coherently across periods."""
    net = _single_bus_network()

    params = StorageParams(
        energy_capacity_mwh=100.0,
        efficiency=0.88,
        soc_initial_mwh=50.0,
        soc_min_mwh=10.0,
        soc_max_mwh=95.0,
    )
    bess_id = net.add_storage(
        bus=1, charge_mw_max=25.0, discharge_mw_max=25.0, params=params
    )
    net.set_generator_cost(bess_id, coeffs=[0.0, 0.0, 0.0])

    net.add_generator(
        bus=1, p_mw=0.0, pmax_mw=100.0, pmin_mw=0.0, vs_pu=1.0,
        qmax_mvar=0.0, qmin_mvar=0.0, machine_id="1",
    )
    net.set_generator_cost("gen_1_2", coeffs=[0.0, 30.0, 0.0])
    net.add_load(bus=1, pd_mw=20.0, qd_mvar=0.0)

    req = {
        "timeline": {"periods": 3, "interval_hours": 1.0},
        "commitment": "all_committed",
        "coupling": "time_coupled",
    }
    result = surge.solve_dispatch(net, req, lp_solver="highs")
    assert result.study["periods"] == 3

    # Because gen_1_2 is expensive ($30/MWh) and the BESS is free, the
    # BESS discharges first until SOC hits the 10 MWh floor, then
    # gen_1_2 serves the remainder.
    storage_soc = []
    gen_power = []
    for p in result.periods:
        for res in p["resource_results"]:
            if res["resource_id"] == bess_id:
                storage_soc.append(res["detail"]["soc_mwh"])
            elif res["resource_id"] == "gen_1_2":
                gen_power.append(res["power_mw"])

    # SOC monotonically decreases (battery only discharges here).
    assert storage_soc == sorted(storage_soc, reverse=True), storage_soc
    # SOC floors at 10 MWh.
    assert min(storage_soc) == pytest.approx(10.0, abs=1e-3)
    # Conventional gen picks up the slack once storage empties.
    assert gen_power[-1] > gen_power[0]
