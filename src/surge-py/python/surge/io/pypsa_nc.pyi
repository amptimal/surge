# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import PathLike
from typing import Union

from .._surge import Network

PathLikeStr = Union[str, "PathLike[str]"]


def load(path: PathLikeStr) -> Network:
    """Load a PyPSA netCDF file into a Surge Network.

    Requires the optional ``pypsa`` package (``pip install pypsa``).
    Preserves per-bus ``v_mag_pu_set`` — the field that the MATPOWER
    export path cannot always round-trip losslessly.

    Converts every standard PyPSA steady-state component (buses,
    lines, transformers, generators, loads, shunt_impedances, links,
    storage_units) into the corresponding Surge model element.
    """
    ...
