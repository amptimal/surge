# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_as, load_as, loads_as, save_as


def load(path):
    """Load a network from a PowSyBl XIIDM (XML IIDM) file.

    Args:
        path: Path to the .xiidm file.

    Returns:
        Network parsed from the XIIDM file.
    """
    return load_as(path, "xiidm")


def loads(content: str):
    """Parse a network from XIIDM XML text content.

    Args:
        content: XIIDM XML string.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "xiidm")


def save(network, path) -> None:
    """Save a network to a PowSyBl XIIDM file.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "xiidm")


def dumps(network) -> str:
    """Serialize a network to XIIDM XML format.

    Args:
        network: Power system network to serialize.

    Returns:
        XIIDM XML string.
    """
    return dumps_as(network, "xiidm")

