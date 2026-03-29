# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import fspath

from .. import _native


def apply_bus_coordinates(network, csv_path) -> int:
    """Apply geographic coordinates to buses from a CSV file.

    The CSV should contain bus number, latitude, and longitude columns.

    Args:
        network: Power system network to update in place.
        csv_path: Path to the CSV file with bus coordinates.

    Returns:
        Number of buses that were successfully matched and updated.
    """
    return _native._io_geo_apply_bus_coordinates(network, fspath(csv_path))

