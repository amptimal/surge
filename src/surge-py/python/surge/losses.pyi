# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from . import Network
from ._surge import AcPfResult, _LsfResult as LsfResult


def compute_loss_factors(
    network: Network,
    solution: AcPfResult | None = None,
) -> LsfResult: ...
