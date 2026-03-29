# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import load_as, loads_as


def load(path):
    """Load a network from an IEEE Common Data Format file.

    Args:
        path: Path to the .cdf file.

    Returns:
        Network parsed from the IEEE CDF file.
    """
    return load_as(path, "cdf")


def loads(content: str):
    """Parse a network from IEEE Common Data Format text content.

    Args:
        content: IEEE CDF text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "cdf")
