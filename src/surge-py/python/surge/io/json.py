# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from ._common import dumps_surge_json, load_as, loads_as, save_surge_json


def load(path):
    """Load a network from a Surge JSON file (optionally zstd-compressed).

    Args:
        path: Path to the .surge.json or .surge.json.zst file.

    Returns:
        Network parsed from the JSON file.
    """
    return load_as(path, "surge-json")


def loads(content: str):
    """Parse a network from Surge JSON text content.

    Args:
        content: Surge JSON string.

    Returns:
        Network parsed from the JSON content.
    """
    return loads_as(content, "surge-json")


def save(network, path, *, pretty: bool = False) -> None:
    """Save a network to a Surge JSON file.

    Args:
        network: Power system network to save.
        path: Destination file path.
        pretty: If True, write human-readable indented JSON.
    """
    save_surge_json(network, path, pretty=pretty)


def dumps(network, *, pretty: bool = False) -> str:
    """Serialize a network to a Surge JSON string.

    Args:
        network: Power system network to serialize.
        pretty: If True, produce indented JSON.

    Returns:
        Surge JSON string representation.
    """
    return dumps_surge_json(network, pretty=pretty)
