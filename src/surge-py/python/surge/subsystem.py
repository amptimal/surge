# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Subsystem — a named filter view over a Network's elements."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ._surge import Network


class Subsystem:
    """A named filter view over a Network's buses, branches, and generators.

    Create a subsystem by specifying any combination of area, zone, kV range,
    or explicit bus list filters.  The resulting bus set is the **intersection**
    of all supplied filters (i.e. a bus must satisfy every criterion).

    Examples::

        # All buses in area 1
        sub = Subsystem(net, areas=[1])

        # 345 kV and above in areas 1 and 2
        sub = Subsystem(net, areas=[1, 2], kv_min=345.0)

        # Explicit bus list
        sub = Subsystem(net, buses=[100, 200, 300])

        # Check membership
        if 118 in sub:
            ...

        # Get element counts
        len(sub)  # number of buses
    """

    def __init__(
        self,
        network: Network,
        name: str = "",
        *,
        areas: list[int] | None = None,
        zones: list[int] | None = None,
        kv_min: float | None = None,
        kv_max: float | None = None,
        buses: list[int] | None = None,
        bus_type: str | None = None,
    ) -> None:
        self._network = network
        self.name = name
        self._bus_set = self._resolve_buses(areas, zones, kv_min, kv_max, buses, bus_type)

    # ------------------------------------------------------------------
    # Bus resolution
    # ------------------------------------------------------------------

    def _resolve_buses(
        self,
        areas: list[int] | None,
        zones: list[int] | None,
        kv_min: float | None,
        kv_max: float | None,
        buses: list[int] | None,
        bus_type: str | None,
    ) -> set[int]:
        net = self._network
        bus_numbers = list(net.bus_numbers)
        bus_area = list(net.bus_area)
        bus_zone = list(net.bus_zone)
        bus_kv = list(net.bus_base_kv)
        bus_type_str = list(net.bus_type_str)

        # Start with all buses, then intersect each filter.
        candidates = set(range(len(bus_numbers)))

        if buses is not None:
            explicit = set(buses)
            candidates &= {i for i in candidates if bus_numbers[i] in explicit}

        if areas is not None:
            area_set = set(areas)
            candidates &= {i for i in candidates if bus_area[i] in area_set}

        if zones is not None:
            zone_set = set(zones)
            candidates &= {i for i in candidates if bus_zone[i] in zone_set}

        if kv_min is not None:
            candidates &= {i for i in candidates if bus_kv[i] >= kv_min}

        if kv_max is not None:
            candidates &= {i for i in candidates if bus_kv[i] <= kv_max}

        if bus_type is not None:
            bt = bus_type.lower()
            candidates &= {i for i in candidates if bus_type_str[i].lower() == bt}

        return {bus_numbers[i] for i in candidates}

    # ------------------------------------------------------------------
    # Properties
    # ------------------------------------------------------------------

    @property
    def network(self) -> Network:
        """The underlying network."""
        return self._network

    @property
    def bus_numbers(self) -> list[int]:
        """Sorted list of bus numbers in the subsystem."""
        return sorted(self._bus_set)

    @property
    def branches(self) -> list[tuple[int, int, int]]:
        """Branches with **both** endpoints in the subsystem.

        Returns list of ``(from_bus, to_bus, circuit)`` tuples.
        """
        net = self._network
        from_buses = list(net.branch_from)
        to_buses = list(net.branch_to)
        circuits = list(net.branch_circuit)
        bs = self._bus_set
        return [
            (from_buses[i], to_buses[i], circuits[i])
            for i in range(len(from_buses))
            if from_buses[i] in bs and to_buses[i] in bs
        ]

    @property
    def tie_branches(self) -> list[tuple[int, int, int]]:
        """Branches with exactly **one** endpoint in the subsystem.

        Returns list of ``(from_bus, to_bus, circuit)`` tuples.
        """
        net = self._network
        from_buses = list(net.branch_from)
        to_buses = list(net.branch_to)
        circuits = list(net.branch_circuit)
        bs = self._bus_set
        return [
            (from_buses[i], to_buses[i], circuits[i])
            for i in range(len(from_buses))
            if (from_buses[i] in bs) != (to_buses[i] in bs)
        ]

    @property
    def generators(self) -> list[tuple[int, str]]:
        """(bus, machine_id) for in-service generators in the subsystem."""
        net = self._network
        gen_buses = list(net.gen_buses)
        gen_ids = list(net.gen_machine_id)
        gen_in_service = list(net.gen_in_service)
        bs = self._bus_set
        return [
            (gen_buses[i], gen_ids[i])
            for i in range(len(gen_buses))
            if gen_buses[i] in bs and gen_in_service[i]
        ]

    @property
    def loads(self) -> list[int]:
        """Bus numbers in the subsystem that carry nonzero load."""
        net = self._network
        bus_numbers = list(net.bus_numbers)
        bus_pd = list(net.bus_pd)
        bus_qd = list(net.bus_qd)
        bs = self._bus_set
        return [
            bus_numbers[i]
            for i in range(len(bus_numbers))
            if bus_numbers[i] in bs and (abs(bus_pd[i]) > 1e-10 or abs(bus_qd[i]) > 1e-10)
        ]

    @property
    def total_load_mw(self) -> float:
        """Total real power load (MW) across subsystem buses."""
        net = self._network
        bus_numbers = list(net.bus_numbers)
        bus_pd = list(net.bus_pd)
        bs = self._bus_set
        return sum(bus_pd[i] for i in range(len(bus_numbers)) if bus_numbers[i] in bs)

    @property
    def total_generation_mw(self) -> float:
        """Total scheduled generation (MW) across subsystem generators."""
        net = self._network
        gen_buses = list(net.gen_buses)
        gen_p = list(net.gen_p)
        gen_in_service = list(net.gen_in_service)
        bs = self._bus_set
        return sum(
            gen_p[i]
            for i in range(len(gen_buses))
            if gen_buses[i] in bs and gen_in_service[i]
        )

    # ------------------------------------------------------------------
    # Dunder methods
    # ------------------------------------------------------------------

    def __len__(self) -> int:
        return len(self._bus_set)

    def __contains__(self, bus: int) -> bool:
        return bus in self._bus_set

    def __repr__(self) -> str:
        name_part = f" {self.name!r}" if self.name else ""
        return (
            f"Subsystem({name_part}, {len(self)} buses, "
            f"{len(self.branches)} branches, "
            f"{len(self.generators)} generators)"
        )
