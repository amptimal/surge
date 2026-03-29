# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import Enum
from os import fspath

from .. import _native
from ._common import load_as, loads_as


class Version(str, Enum):
    V2_4_15 = "2.4.15"
    V3_0 = "3.0"


Profiles = _native._CgmesProfiles


def _version_name(version: Version | str) -> str:
    if isinstance(version, Version):
        return version.value
    return str(version)


def load(path):
    """Load a network from a CGMES (CIM) XML file or directory.

    Args:
        path: Path to the CGMES file or directory.

    Returns:
        Network parsed from the CGMES data.
    """
    return load_as(path, "cgmes")


def loads(content: str):
    """Parse a network from CGMES XML string content.

    Args:
        content: CGMES XML text.

    Returns:
        Network parsed from the CGMES content.
    """
    return loads_as(content, "cgmes")


def save(network, output_dir, *, version: Version | str = Version.V2_4_15) -> None:
    """Export a network to CGMES XML files in the specified output directory.

    Args:
        network: Power system network to export.
        output_dir: Directory where CGMES profile files will be written.
        version: CGMES version (default "2.4.15").
    """
    _native._io_cgmes_save(network, fspath(output_dir), _version_name(version))


def to_profiles(network, *, version: Version | str = Version.V2_4_15) -> Profiles:
    """Convert a network to in-memory CGMES profile strings.

    Args:
        network: Power system network to convert.
        version: CGMES version (default "2.4.15").

    Returns:
        Profiles object with per-profile XML strings.
    """
    return _native._io_cgmes_to_profiles(network, _version_name(version))
