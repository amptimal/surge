# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import IntEnum

from .._common import dumps_as, load_as, loads_as, save_as


class Version(IntEnum):
    V33 = 33
    V34 = 34
    V35 = 35
    V36 = 36


def load(path):
    """Load a network from a PSS/E RAW file.

    Args:
        path: Path to the .raw file.

    Returns:
        Network parsed from the PSS/E RAW file.
    """
    return load_as(path, "psse")


def loads(content: str):
    """Parse a network from PSS/E RAW text content.

    Args:
        content: PSS/E RAW file text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "psse")


def save(network, path, *, version: int | Version = Version.V33) -> None:
    """Save a network to a PSS/E RAW file.

    Args:
        network: Power system network to save.
        path: Destination file path.
        version: PSS/E version (default V33). Supports V33 through V36.
    """
    save_as(network, path, "psse", int(version))


def dumps(network, *, version: int | Version = Version.V33) -> str:
    """Serialize a network to PSS/E RAW text format.

    Args:
        network: Power system network to serialize.
        version: PSS/E version (default V33). Supports V33 through V36.

    Returns:
        PSS/E RAW format string.
    """
    return dumps_as(network, "psse", int(version))

