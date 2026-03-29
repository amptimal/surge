# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from . import Network


def merge_networks(
    net1: Network,
    net2: Network,
    tie_buses: list[tuple[int, int]] | None = None,
) -> Network: ...
