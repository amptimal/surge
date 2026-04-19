# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solver / formulation policy for a day-ahead RTO market solve."""

from __future__ import annotations

from dataclasses import dataclass


DEFAULT_VOLL_PER_MWH = 9_000.0
DEFAULT_THERMAL_OVERLOAD_COST_PER_MWH = 5_000.0
DEFAULT_RESERVE_SHORTFALL_COST_PER_MWH = 1_000.0


@dataclass(frozen=True)
class RtoPolicy:
    """Day-ahead RTO market policy knobs.

    The market is DC-only, four-product AS (reg-up, reg-down, spin,
    non-spin), with LMP repricing on by default. Penalty defaults
    mirror typical ISO scarcity pricing (VOLL ≈ $9 000/MWh).
    """

    # Solver
    lp_solver: str = "highs"
    mip_gap: float = 1e-3
    time_limit_secs: float | None = None

    # Commitment
    commitment_mode: str = "optimize"  # "optimize" (MIP) | "all_committed" (LP only)

    # Pricing
    run_pricing: bool = True  # LMP repricing LP after SCUC

    # Penalty tensor ($/MWh — scaled by base_mva at config-build time)
    voll_per_mwh: float = DEFAULT_VOLL_PER_MWH
    thermal_overload_cost_per_mwh: float = DEFAULT_THERMAL_OVERLOAD_COST_PER_MWH
    reserve_shortfall_cost_per_mwh: float = DEFAULT_RESERVE_SHORTFALL_COST_PER_MWH

    # Logging
    log_level: str = "info"


__all__ = [
    "DEFAULT_RESERVE_SHORTFALL_COST_PER_MWH",
    "DEFAULT_THERMAL_OVERLOAD_COST_PER_MWH",
    "DEFAULT_VOLL_PER_MWH",
    "RtoPolicy",
]
