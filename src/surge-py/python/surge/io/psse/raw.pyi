# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import IntEnum
from os import PathLike

from ... import Network


class Version(IntEnum):
    V33 = 33
    V34 = 34
    V35 = 35
    V36 = 36


def load(path: str | PathLike[str]) -> Network: ...
def loads(content: str) -> Network: ...
def save(
    network: Network,
    path: str | PathLike[str],
    *,
    version: int | Version = Version.V33,
) -> None: ...
def dumps(network: Network, *, version: int | Version = Version.V33) -> str: ...
