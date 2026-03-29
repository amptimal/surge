# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import PathLike


def read_load_profiles_csv(path: str | PathLike[str]) -> dict[int, list[float]]: ...
def read_renewable_profiles_csv(path: str | PathLike[str]) -> dict[str, list[float]]: ...
