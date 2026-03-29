# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

import os

from .. import _native


def read_load_profiles_csv(path):
    """Read time-series load profiles from a CSV file.

    Args:
        path: Path to the CSV file with load profile data.

    Returns:
        Load profile data suitable for use in dispatch and market simulations.
    """
    return _native._io_profiles_read_load_csv(os.fspath(path))


def read_renewable_profiles_csv(path):
    """Read time-series renewable generation profiles from a CSV file.

    Args:
        path: Path to the CSV file with renewable profile data.

    Returns:
        Renewable profile data suitable for use in dispatch and market simulations.
    """
    return _native._io_profiles_read_renewable_csv(os.fspath(path))


__all__ = ["read_load_profiles_csv", "read_renewable_profiles_csv"]
