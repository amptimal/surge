# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import Enum

from .. import _native
from . import bin, cgmes, dss, epc, export, geo, ieee_cdf, json, matpower, profiles, psse, ucte, xiidm


class Format(str, Enum):
    MATPOWER = "matpower"
    PSSE_RAW = "psse"
    XIIDM = "xiidm"
    UCTE = "ucte"
    SURGE_JSON = "surge-json"
    DSS = "dss"
    EPC = "epc"


def _format_name(format: Format | str) -> str:
    if isinstance(format, Format):
        return format.value
    return str(format)


def loads(content: str, format: Format | str):
    """Parse a network from a string in the specified format.

    Args:
        content: Text content of the case file.
        format: File format (e.g. Format.MATPOWER, "psse", "surge-json").

    Returns:
        Network parsed from the content string.
    """
    return _native._loads(content, _format_name(format))


def dumps(network, format: Format | str, *, version: int | None = None) -> str:
    """Serialize a network to a string in the specified format.

    Args:
        network: Power system network to serialize.
        format: Output format (e.g. Format.MATPOWER, "psse", "surge-json").
        version: Optional format version number (e.g. 33 for PSS/E v33).

    Returns:
        String representation of the network in the requested format.
    """
    return _native._dumps(network, _format_name(format), version)


__all__ = [
    "Format",
    "bin",
    "cgmes",
    "dss",
    "dumps",
    "epc",
    "export",
    "geo",
    "ieee_cdf",
    "json",
    "loads",
    "matpower",
    "profiles",
    "psse",
    "ucte",
    "xiidm",
]
