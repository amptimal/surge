# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Build a Surge Network from pandas DataFrames.

Follows MATPOWER column conventions for familiarity.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import pandas as pd


def _row_value(row, key: str, default, *alt_keys: str):
    """Return a row value, treating pandas missing values as absent.

    Tries *key* first, then each alternate key in *alt_keys*.
    """
    import pandas as pd

    for k in (key, *alt_keys):
        value = row.get(k, None)
        if value is not None:
            try:
                if pd.isna(value):
                    continue
            except (TypeError, ValueError):
                pass
            return value
    return default


def _required_row_value(row, key: str, *alt_keys: str):
    """Return a required row value or raise a clear schema/value error."""
    import pandas as pd

    for k in (key, *alt_keys):
        if k not in row:
            continue
        value = row.get(k, None)
        if value is None:
            raise ValueError(f"required field {k!r} is missing")
        try:
            if pd.isna(value):
                raise ValueError(f"required field {k!r} is missing")
        except (TypeError, ValueError):
            pass
        return value
    raise KeyError(key)


def from_dataframes(
    buses: "pd.DataFrame",
    branches: "pd.DataFrame",
    generators: "pd.DataFrame",
    *,
    base_mva: float = 100.0,
    name: str = "",
) -> "Network":
    """Build a Network from pandas DataFrames.

    Accepts both short MATPOWER-style column names and the unit-suffixed
    names produced by ``Network.bus_dataframe()``, ``branch_dataframe()``,
    and ``gen_dataframe()``, so a round-trip works out of the box::

        net2 = from_dataframes(
            net.bus_dataframe(), net.branch_dataframe(), net.gen_dataframe(),
            base_mva=net.base_mva,
        )

    Parameters
    ----------
    buses : DataFrame
        Required columns: ``number`` (or use the DataFrame index), ``base_kv``.
        Optional: ``type`` (PQ/PV/Slack, default PQ), ``name``,
        ``pd_mw`` (default 0), ``qd_mvar`` (default 0),
        ``vm_pu`` (default 1.0), ``va_deg`` (default 0.0).
    branches : DataFrame
        Required columns: ``from_bus``, ``to_bus``, ``r``, ``x``.
        Optional: ``b`` (default 0), ``rate_a`` or ``rate_a_mva`` (default 0),
        ``tap`` (default 1), ``shift`` or ``shift_deg`` (default 0),
        ``circuit`` (default 1).
    generators : DataFrame
        Required columns: ``bus`` (or use the DataFrame index),
        ``pg`` or ``p_mw``.
        Optional: ``pmax``/``pmax_mw`` (default 9999),
        ``pmin``/``pmin_mw`` (default 0),
        ``qmax``/``qmax_mvar`` (default 9999),
        ``qmin``/``qmin_mvar`` (default -9999),
        ``vs``/``vs_pu`` (default 1.0), ``machine_id`` (default "1").
    base_mva : float
        System base MVA (default 100).
    name : str
        Network name.

    Returns
    -------
    surge.Network
    """
    from ._surge import Network

    # Flatten any index columns so bus_id, from_bus, to_bus, etc. are
    # available as regular columns regardless of how the DataFrame was built.
    buses = buses.reset_index()
    branches = branches.reset_index()
    generators = generators.reset_index()

    net = Network(name=name, base_mva=base_mva)

    # --- Buses ---
    for _, row in buses.iterrows():
        # Accept "number" or "bus_id" (the index name from bus_dataframe()).
        number = int(_required_row_value(row, "number", "bus_id"))
        base_kv = float(_required_row_value(row, "base_kv"))
        bus_type = str(_row_value(row, "type", "PQ"))
        bus_name = str(_row_value(row, "name", ""))
        pd_mw = float(_row_value(row, "pd_mw", 0.0))
        qd_mvar = float(_row_value(row, "qd_mvar", 0.0))
        vm_pu = float(_row_value(row, "vm_pu", 1.0))
        va_deg = float(_row_value(row, "va_deg", 0.0))
        net.add_bus(
            number, bus_type, base_kv,
            name=bus_name, pd_mw=pd_mw, qd_mvar=qd_mvar,
            vm_pu=vm_pu, va_deg=va_deg,
        )

    # --- Branches ---
    for _, row in branches.iterrows():
        from_bus = int(_required_row_value(row, "from_bus"))
        to_bus = int(_required_row_value(row, "to_bus"))
        r = float(_required_row_value(row, "r"))
        x = float(_required_row_value(row, "x"))
        b = float(_row_value(row, "b", 0.0))
        rate_a = float(_row_value(row, "rate_a", 0.0, "rate_a_mva"))
        tap = float(_row_value(row, "tap", 1.0))
        shift = float(_row_value(row, "shift", 0.0, "shift_deg"))
        circuit = int(_row_value(row, "circuit", 1))
        net.add_branch(
            from_bus, to_bus, r=r, x=x, b=b,
            rate_a_mva=rate_a, tap=tap, shift_deg=shift, circuit=circuit,
        )

    # --- Generators ---
    for _, row in generators.iterrows():
        # Accept "bus" or "bus_id" (the index name from gen_dataframe()).
        bus = int(_required_row_value(row, "bus", "bus_id"))
        pg = float(_required_row_value(row, "pg", "p_mw"))
        pmax = float(_row_value(row, "pmax", 9999.0, "pmax_mw"))
        pmin = float(_row_value(row, "pmin", 0.0, "pmin_mw"))
        qmax = float(_row_value(row, "qmax", 9999.0, "qmax_mvar"))
        qmin = float(_row_value(row, "qmin", -9999.0, "qmin_mvar"))
        vs = float(_row_value(row, "vs", 1.0, "vs_pu"))
        machine_id = str(_row_value(row, "machine_id", "1"))
        net.add_generator(
            bus, p_mw=pg, pmax_mw=pmax, pmin_mw=pmin,
            vs_pu=vs, qmax_mvar=qmax, qmin_mvar=qmin,
            machine_id=machine_id,
        )

    return net
