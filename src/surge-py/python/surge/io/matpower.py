# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_as, load_as, loads_as, save_as


def load(path):
    """Load a network from a MATPOWER .m case file.

    Args:
        path: Path to the MATPOWER .m file.

    Returns:
        Network parsed from the MATPOWER file.
    """
    return load_as(path, "matpower")


def loads(content: str):
    """Parse a network from MATPOWER case text content.

    Args:
        content: MATPOWER .m file text.

    Returns:
        Network parsed from the content.
    """
    return loads_as(content, "matpower")


def save(network, path) -> None:
    """Save a network to a MATPOWER .m case file.

    Args:
        network: Power system network to save.
        path: Destination file path.
    """
    save_as(network, path, "matpower")


def dumps(network) -> str:
    """Serialize a network to MATPOWER .m case format.

    Args:
        network: Power system network to serialize.

    Returns:
        MATPOWER .m case file string.
    """
    return dumps_as(network, "matpower")

