# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_as, load_as, loads_as, save_as


def load(path):
    """Load a network from a PowerWorld EPC file.

    Args:
        path: Path to the .epc file.

    Returns:
        Network parsed from the EPC file.
    """
    return load_as(path, "epc")


def loads(content: str):
    """Parse a network from PowerWorld EPC text content.

    Args:
        content: EPC file text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "epc")


def save(network, path) -> None:
    """Save a network to a PowerWorld EPC file.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "epc")


def dumps(network) -> str:
    """Serialize a network to PowerWorld EPC text format.

    Args:
        network: Power system network to serialize.

    Returns:
        EPC format string.
    """
    return dumps_as(network, "epc")

