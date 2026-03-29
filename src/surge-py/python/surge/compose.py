# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from . import _native


def merge_networks(net1, net2, tie_buses=None):
    """Merge two power system networks into a single combined network.

    Args:
        net1: First network.
        net2: Second network.
        tie_buses: Optional list of bus numbers that connect the two networks.
            When None, buses with matching numbers are automatically tied.

    Returns:
        A new merged Network containing all buses, branches, and generators
        from both inputs.
    """
    return _native._compose_merge_networks(net1, net2, tie_buses)
