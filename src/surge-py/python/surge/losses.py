# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from . import _native


LsfResult = _native._LsfResult


def compute_loss_factors(network, solution=None) -> LsfResult:
    """Compute AC marginal loss sensitivity factors for each bus.

    Uses an analytical Jacobian-transpose solve — approximately the cost of
    one Newton-Raphson iteration. If ``solution`` is provided, that AC
    operating point is used directly; otherwise a base AC power flow is
    solved internally.

    Args:
        network: Power system network.
        solution: Optional AcPfResult to use as the operating point.

    Returns:
        LsfResult with per-bus loss factors and base losses.
    """
    return _native._losses_compute_factors(network, solution)


__all__ = ["LsfResult", "compute_loss_factors"]
