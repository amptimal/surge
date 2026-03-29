# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from os import PathLike

from .. import Network


def apply_bus_coordinates(network: Network, csv_path: str | PathLike[str]) -> int: ...
