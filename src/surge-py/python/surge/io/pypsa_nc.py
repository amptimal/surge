# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""PyPSA netCDF bridge.

Loads a PyPSA netCDF file via the optional ``pypsa`` package and builds a
Surge :class:`Network` directly from PyPSA's component tables. This
preserves PyPSA-native voltage-setpoint state (per-bus
``v_mag_pu_set``) that the MATPOWER / pandapower round-trip paths
cannot always carry through.

This module is optional — it only imports ``pypsa`` when a function is
called. Install with ``pip install pypsa`` to enable.

Usage
-----

.. code-block:: python

    import surge
    # Requires `pip install pypsa`
    net = surge.io.pypsa_nc.load("path/to/case.nc")
    sol = surge.solve_ac_pf(net)

Components covered
------------------

The bridge converts every standard PyPSA component that has a direct
analogue in Surge's steady-state domain model:

- ``Bus`` → ``surge.Bus`` — preserves ``v_mag_pu_set``, ``v_ang_set``,
  nominal voltage, and derives Slack / PV / PQ from attached
  generators' ``control`` column.
- ``Line`` → ``surge.Branch`` (Line) — r, x (ohms), b (siemens)
  rebased to per-unit on the system base using ``z_base = v_nom² /
  base_mva``. ``s_nom`` → ``rate_a_mva``.
- ``Transformer`` → ``surge.Branch`` (Transformer) — r, x, b stored by
  PyPSA in per-unit on the transformer's own ``s_nom`` base; rebased
  to the system base via ``base_mva / s_nom``. ``tap_ratio`` and
  ``phase_shift`` preserved verbatim.
- ``Generator`` → ``surge.Generator`` — ``p_set`` → dispatch,
  ``p_nom`` → ``pmax_mw``, ``p_nom_min`` → ``pmin_mw``. Generator
  voltage setpoint comes from the connected bus's ``v_mag_pu_set``.
  Q-limits: passed through as ``9999/-9999`` (PyPSA's AC PF does not
  carry per-generator Q limits by default).
- ``Load`` → ``surge.Load`` — one surge Load per PyPSA Load, preserving
  the PyPSA name as ``load_id``.
- ``ShuntImpedance`` → ``surge.FixedShunt`` — rebased from siemens to
  MW/MVAr on the system base.
- Bus-level PyPower-origin shunts (``Bus.Gs`` / ``Bus.Bs`` columns,
  non-zero) → ``surge.FixedShunt`` attached to that bus.
- ``Link`` → ``surge.VscHvdcLink`` — point-to-point HVDC equivalent,
  using ``p_nom`` as MW setpoint (or ``p_set`` when present).
- ``StorageUnit`` → ``surge.StorageParams`` + ``surge.add_storage`` —
  energy capacity derived from ``p_nom × max_hours``, charge / discharge
  efficiency from ``efficiency_store`` / ``efficiency_dispatch``.

PyPSA features that are ``time-series-only`` (e.g. ``loads_t.p_set``
time-dependent load schedules) are not represented in Surge's
steady-state model; the bridge reads the scalar ``p_set`` / ``q_set``
at the first snapshot.

Solver-convention note
----------------------

Surge's AC solver may converge on a slightly different operating point
than PyPSA's ``pf()`` on the same network. This is the same class of
divergence documented in ``format-interop.md``: perfect numerical
agreement across solvers on an identical nominal case is rare. The
bridge faithfully carries inputs; the solve is Surge's.

See also
--------

- :doc:`/format-interop` for when to prefer this path over MATPOWER.
"""
from __future__ import annotations

from os import PathLike
from typing import Any, Dict, TYPE_CHECKING, Union

if TYPE_CHECKING:
    from .._surge import Network


PathLikeStr = Union[str, "PathLike[str]"]


def _require_pypsa():
    try:
        import pypsa
    except ImportError as exc:  # pragma: no cover - guarded import
        raise ImportError(
            "surge.io.pypsa_nc requires the optional 'pypsa' package. "
            "Install with: pip install pypsa"
        ) from exc
    return pypsa


def _bus_number(name: Any) -> int:
    """Coerce a PyPSA bus name to an integer bus number."""
    try:
        return int(name)
    except (TypeError, ValueError) as exc:
        raise ValueError(
            f"PyPSA bus name {name!r} is not a decimal integer. "
            "surge.io.pypsa_nc currently requires integer-named buses "
            "(the convention when PyPSA networks originate from PyPower "
            "or MATPOWER). Rename buses to integer strings before load."
        ) from exc


def _circuit_from_name(name: Any, strip_prefix: str = "") -> int:
    """Parse a PyPSA component name to an integer circuit id."""
    try:
        return int(str(name).lstrip(strip_prefix))
    except (TypeError, ValueError):
        return 1


_BUS_CONTROL_TO_SURGE_TYPE = {
    "slack": "Slack",
    "pv": "PV",
    "pq": "PQ",
}


def _resolve_bus_type(control: Any) -> str:
    if control is None:
        return "PQ"
    key = str(control).strip().lower()
    return _BUS_CONTROL_TO_SURGE_TYPE.get(key, "PQ")


def _nz(value: Any, default: float = 0.0) -> float:
    """Convert to float, treating None / NaN as the default."""
    if value is None:
        return default
    try:
        if value != value:  # NaN
            return default
    except TypeError:
        return default
    return float(value)


def load(path: PathLikeStr) -> "Network":
    """Load a PyPSA netCDF file into a Surge :class:`Network`.

    Preserves per-bus voltage setpoints (``v_mag_pu_set``) — the field
    that the MATPOWER export path cannot always round-trip losslessly.

    Args:
        path: Filesystem path to a PyPSA netCDF (``.nc``).

    Returns:
        surge.Network: constructed with the same topology, generator
        dispatch, and load state as the PyPSA network.

    Raises:
        ImportError: If the ``pypsa`` package is not installed.
        ValueError: If any bus name in the PyPSA network is not a
            decimal integer.
    """
    pypsa = _require_pypsa()
    from .._surge import FixedShunt, Network, StorageParams

    pypsa_net = pypsa.Network(str(path))
    base_mva = 100.0
    net = Network(name=str(getattr(pypsa_net, "name", "") or ""), base_mva=base_mva)

    # Cache bus nominal voltages for line SI→pu conversion. PyPSA's
    # lines store r, x in ohms and b in siemens; we rebase to per-unit
    # on the system base.
    bus_vnom: Dict[str, float] = {
        str(name): _nz(row.get("v_nom", 0.0))
        for name, row in pypsa_net.buses.iterrows()
    }

    # Derive surge bus types from the generator control column (not the
    # bus's own control — PyPSA's per-bus control is often just "PQ"
    # regardless of attached generators). One generator with
    # control="Slack" → its bus is the surge Slack; generators with
    # control="PV" mark their bus as PV; everything else PQ.
    bus_type_override: Dict[str, str] = {}
    for _, gen_row in pypsa_net.generators.iterrows():
        gen_control = str(gen_row.get("control", "") or "").strip().lower()
        gen_bus = str(gen_row["bus"])
        if gen_control == "slack":
            bus_type_override[gen_bus] = "Slack"
        elif gen_control == "pv" and bus_type_override.get(gen_bus) != "Slack":
            bus_type_override[gen_bus] = "PV"

    # --- Buses ----------------------------------------------------------
    for bus_name, row in pypsa_net.buses.iterrows():
        number = _bus_number(bus_name)
        base_kv = _nz(row.get("v_nom", 0.0))
        bus_type = bus_type_override.get(
            str(bus_name), _resolve_bus_type(row.get("control"))
        )
        vm_pu = _nz(row.get("v_mag_pu_set", 1.0), default=1.0)
        va_deg = _nz(row.get("v_ang_set", 0.0))
        net.add_bus(
            number,
            bus_type,
            base_kv,
            name=str(bus_name),
            pd_mw=0.0,
            qd_mvar=0.0,
            vm_pu=vm_pu,
            va_deg=va_deg,
        )

    # --- Lines ----------------------------------------------------------
    # PyPSA (from-PyPower origin) stores r, x in OHMS, b in SIEMENS
    # (total line, not per-km). Rebase to per-unit on system base.
    for line_name, row in pypsa_net.lines.iterrows():
        from_bus = _bus_number(row["bus0"])
        to_bus = _bus_number(row["bus1"])
        r_ohm = _nz(row.get("r", 0.0))
        x_ohm = _nz(row.get("x", 0.0))
        b_siemens = _nz(row.get("b", 0.0))
        v_nom = bus_vnom.get(str(row["bus0"]), 0.0) or bus_vnom.get(
            str(row["bus1"]), 0.0
        )
        if v_nom > 0.0:
            z_base = v_nom * v_nom / base_mva
            r_pu = r_ohm / z_base
            x_pu = x_ohm / z_base
            b_pu = b_siemens * z_base
        else:
            r_pu, x_pu, b_pu = r_ohm, x_ohm, b_siemens
        s_nom = _nz(row.get("s_nom", 0.0))
        circuit = _circuit_from_name(line_name, strip_prefix="L")
        net.add_branch(
            from_bus,
            to_bus,
            r=r_pu,
            x=x_pu,
            b=b_pu,
            rate_a_mva=s_nom,
            tap=1.0,
            shift_deg=0.0,
            circuit=circuit,
        )

    # --- Transformers ---------------------------------------------------
    # PyPSA stores r, x in per-unit on the TRANSFORMER's s_nom base;
    # rebase to the system base: r_sys = r_t * base_mva / s_nom.
    for xfmr_name, row in pypsa_net.transformers.iterrows():
        from_bus = _bus_number(row["bus0"])
        to_bus = _bus_number(row["bus1"])
        r_xfmr = _nz(row.get("r", 0.0))
        x_xfmr = _nz(row.get("x", 0.0))
        b_xfmr = _nz(row.get("b", 0.0))
        s_nom = _nz(row.get("s_nom", 0.0))
        if s_nom > 0.0:
            scale = base_mva / s_nom
            r_pu = r_xfmr * scale
            x_pu = x_xfmr * scale
            b_pu = b_xfmr / scale
        else:
            r_pu, x_pu, b_pu = r_xfmr, x_xfmr, b_xfmr
        tap = _nz(row.get("tap_ratio", 1.0), default=1.0)
        phase_shift = _nz(row.get("phase_shift", 0.0))
        circuit = _circuit_from_name(xfmr_name, strip_prefix="T")
        net.add_branch(
            from_bus,
            to_bus,
            r=r_pu,
            x=x_pu,
            b=b_pu,
            rate_a_mva=s_nom,
            tap=tap,
            shift_deg=phase_shift,
            circuit=circuit,
        )

    # --- Generators -----------------------------------------------------
    # PyPSA's generators table doesn't carry per-gen Q limits for
    # standard PF use; PyPSA's AC solver handles Q via bus-type
    # switching. We pass large Q limits (±9999) to match PyPSA's
    # effectively-unbounded default.
    for gen_name, row in pypsa_net.generators.iterrows():
        bus = _bus_number(row["bus"])
        p_mw = _nz(row.get("p_set", 0.0))
        p_nom = _nz(row.get("p_nom", 0.0))
        p_min = _nz(row.get("p_nom_min", 0.0))
        gen_vm = row.get("vm_pu_set", None)
        if gen_vm is None or gen_vm != gen_vm:
            gen_vm = _nz(
                pypsa_net.buses.at[str(row["bus"]), "v_mag_pu_set"], default=1.0
            )
        else:
            gen_vm = float(gen_vm)
        net.add_generator(
            bus,
            p_mw=p_mw,
            pmax_mw=p_nom,
            pmin_mw=p_min,
            vs_pu=gen_vm,
            qmax_mvar=9999.0,
            qmin_mvar=-9999.0,
            machine_id=str(gen_name),
        )

    # --- Loads ----------------------------------------------------------
    for load_name, row in pypsa_net.loads.iterrows():
        bus = _bus_number(row["bus"])
        p_mw = _nz(row.get("p_set", 0.0))
        q_mvar = _nz(row.get("q_set", 0.0))
        net.add_load(
            bus=bus,
            pd_mw=p_mw,
            qd_mvar=q_mvar,
            load_id=str(load_name),
            conforming=True,
        )

    # --- Shunts ---------------------------------------------------------
    # Two sources of shunts in PyPSA:
    #   (1) ShuntImpedance components (preferred; explicit)
    #   (2) Legacy Bus.Gs / Bus.Bs columns populated by PyPower-origin
    #       networks. These may be zero but when non-zero they're real
    #       shunt contributions that would otherwise be lost.
    for shunt_name, row in pypsa_net.shunt_impedances.iterrows():
        bus_s = str(row["bus"])
        bus = _bus_number(bus_s)
        v_nom = bus_vnom.get(bus_s, 0.0)
        # PyPSA: g, b are in siemens; g_pu, b_pu are system-pu derived.
        # Surge FixedShunt expects g_mw and b_mvar at 1.0 pu voltage on
        # the system base: g_mw = g_pu * base_mva, b_mvar = b_pu * base_mva.
        g_pu = _nz(row.get("g_pu", 0.0))
        b_pu = _nz(row.get("b_pu", 0.0))
        if g_pu == 0.0 and b_pu == 0.0:
            g = _nz(row.get("g", 0.0))
            b = _nz(row.get("b", 0.0))
            if v_nom > 0.0:
                z_base = v_nom * v_nom / base_mva
                g_pu = g * z_base  # S × Z = dimensionless (pu)
                b_pu = b * z_base
        g_mw = g_pu * base_mva
        b_mvar = b_pu * base_mva
        if g_mw == 0.0 and b_mvar == 0.0:
            continue
        shunt = FixedShunt(
            bus=bus,
            id=str(shunt_name),
            shunt_type="Capacitor" if b_mvar >= 0 else "Reactor",
            g_mw=g_mw,
            b_mvar=b_mvar,
        )
        net.add_fixed_shunt_object(shunt)

    # Legacy bus-level shunts from the PyPower import path.
    for bus_name, row in pypsa_net.buses.iterrows():
        gs = _nz(row.get("Gs", 0.0))
        bs = _nz(row.get("Bs", 0.0))
        if gs == 0.0 and bs == 0.0:
            continue
        bus = _bus_number(bus_name)
        shunt = FixedShunt(
            bus=bus,
            id=f"bus_shunt_{bus_name}",
            shunt_type="Capacitor" if bs >= 0 else "Reactor",
            g_mw=gs,
            b_mvar=bs,
        )
        net.add_fixed_shunt_object(shunt)

    # --- HVDC Links -----------------------------------------------------
    # PyPSA Links are generic bus-to-bus energy flows. The most common
    # use for links in a transmission steady-state context is as an
    # HVDC point-to-point connection, which maps to surge's VSC HVDC
    # line. We use p_set (or p_nom when p_set is absent) as the
    # scheduled MW.
    for link_name, row in pypsa_net.links.iterrows():
        bus_f = _bus_number(row["bus0"])
        bus_t = _bus_number(row["bus1"])
        p_nom = _nz(row.get("p_nom", 0.0))
        p_set = _nz(row.get("p_set", 0.0))
        mw_setpoint = p_set if p_set != 0.0 else p_nom
        # PyPSA doesn't store a per-side Q limit on Links. Use a
        # symmetric ±p_nom/2 heuristic which is typical for a VSC
        # converter (Q capability ~ half of its rated P).
        q_lim = p_nom * 0.5 if p_nom > 0.0 else 9999.0
        # Use the connected buses' voltage setpoints (they've been
        # loaded above) as AC-side references.
        ac_vm_f = _nz(
            pypsa_net.buses.at[str(row["bus0"]), "v_mag_pu_set"], default=1.0
        )
        ac_vm_t = _nz(
            pypsa_net.buses.at[str(row["bus1"]), "v_mag_pu_set"], default=1.0
        )
        net.add_vsc_dc_line(
            name=str(link_name),
            bus_f=bus_f,
            bus_t=bus_t,
            mw_setpoint=mw_setpoint,
            ac_vm_f=ac_vm_f,
            ac_vm_t=ac_vm_t,
            q_min_mvar=-q_lim,
            q_max_mvar=q_lim,
        )

    # --- Storage Units --------------------------------------------------
    for storage_name, row in pypsa_net.storage_units.iterrows():
        bus = _bus_number(row["bus"])
        p_nom = _nz(row.get("p_nom", 0.0))
        max_hours = _nz(row.get("max_hours", 1.0), default=1.0)
        energy_capacity_mwh = p_nom * max_hours
        eff_store = _nz(row.get("efficiency_store", 1.0), default=1.0)
        eff_dispatch = _nz(row.get("efficiency_dispatch", 1.0), default=1.0)
        soc_initial = _nz(
            row.get("state_of_charge_initial", energy_capacity_mwh * 0.5),
            default=energy_capacity_mwh * 0.5,
        )
        params = StorageParams(
            energy_capacity_mwh=energy_capacity_mwh,
            charge_efficiency=eff_store,
            discharge_efficiency=eff_dispatch,
            soc_initial_mwh=soc_initial,
            soc_min_mwh=0.0,
            soc_max_mwh=energy_capacity_mwh,
        )
        p_min_pu = _nz(row.get("p_min_pu", -1.0), default=-1.0)
        p_max_pu = _nz(row.get("p_max_pu", 1.0), default=1.0)
        charge_mw_max = abs(p_min_pu) * p_nom if p_min_pu < 0 else 0.0
        discharge_mw_max = p_max_pu * p_nom if p_max_pu > 0 else p_nom
        net.add_storage(
            bus=bus,
            charge_mw_max=charge_mw_max,
            discharge_mw_max=discharge_mw_max,
            params=params,
            machine_id=str(storage_name),
        )

    # Ensure canonical IDs for downstream market / dispatch tooling.
    try:
        net.canonicalize_runtime_identities()
    except AttributeError:
        pass

    return net


__all__ = ["load"]
