# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from .._common import load_as, loads_as


def load(path):
    """Load a network from a PSS/E RAWX (JSON-based) file.

    Args:
        path: Path to the .rawx file.

    Returns:
        Network parsed from the RAWX file.
    """
    return load_as(path, "rawx")


def loads(content: str):
    """Parse a network from PSS/E RAWX JSON text content.

    Args:
        content: RAWX JSON text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "rawx")

