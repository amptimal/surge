# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_as, load_as, loads_as, save_as


def load(path):
    """Load a network from a UCTE-DEF exchange format file.

    Args:
        path: Path to the .uct file.

    Returns:
        Network parsed from the UCTE file.
    """
    return load_as(path, "ucte")


def loads(content: str):
    """Parse a network from UCTE-DEF text content.

    Args:
        content: UCTE-DEF text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "ucte")


def save(network, path) -> None:
    """Save a network to a UCTE-DEF exchange format file.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "ucte")


def dumps(network) -> str:
    """Serialize a network to UCTE-DEF text format.

    Args:
        network: Power system network to serialize.

    Returns:
        UCTE-DEF format string.
    """
    return dumps_as(network, "ucte")

