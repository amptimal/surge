# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_as, load_as, loads_as, save_as


def load(path):
    """Load a network from an OpenDSS file.

    Args:
        path: Path to the .dss file.

    Returns:
        Network parsed from the OpenDSS file.
    """
    return load_as(path, "dss")


def loads(content: str):
    """Parse a network from OpenDSS text content.

    Args:
        content: OpenDSS script text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "dss")


def save(network, path) -> None:
    """Save a network to an OpenDSS file.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "dss")


def dumps(network) -> str:
    """Serialize a network to OpenDSS text format.

    Args:
        network: Power system network to serialize.

    Returns:
        OpenDSS script string.
    """
    return dumps_as(network, "dss")

