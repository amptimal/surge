# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import Enum
from os import PathLike

from .. import Network
from .._surge import _CgmesProfiles as Profiles


class Version(str, Enum):
    V2_4_15 = "2.4.15"
    V3_0 = "3.0"


def load(path: str | PathLike[str]) -> Network: ...
def loads(content: str) -> Network: ...
def save(
    network: Network,
    output_dir: str | PathLike[str],
    *,
    version: Version | str = Version.V2_4_15,
) -> None: ...
def to_profiles(
    network: Network,
    *,
    version: Version | str = Version.V2_4_15,
) -> Profiles: ...
