# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_bytes_as, load_as, loads_bytes_as, save_as


def load(path):
    """Load a network from a Surge binary file on disk.

    Args:
        path: Path to the .surge.bin file.

    Returns:
        Network parsed from the binary file.
    """
    return load_as(path, "surge-bin")


def loads(content: bytes):
    """Parse a network from Surge binary bytes.

    Args:
        content: Raw bytes of a Surge binary file.

    Returns:
        Network parsed from the binary content.
    """
    return loads_bytes_as(content, "surge-bin")


def save(network, path) -> None:
    """Save a network to a Surge binary file on disk.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "surge-bin")


def dumps(network) -> bytes:
    """Serialize a network to Surge binary bytes.

    Args:
        network: Power system network to serialize.

    Returns:
        Raw bytes of the Surge binary representation.
    """
    return dumps_bytes_as(network, "surge-bin")
