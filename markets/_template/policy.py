# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solver / formulation policy knobs for this market.

Keep this dataclass small and declarative. Fields should answer:

* Which LP / MIP solver?
* Commitment mode and MIP tuning?
* Penalty multipliers specific to this market's scoring?
* Logging verbosity?

If you find yourself adding an *algorithm* flag, ask whether that
belongs here (user-facing, stable) or inside :func:`solve` (an
implementation detail that might change).
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Policy:
    """User-facing policy knobs."""

    lp_solver: str = "highs"
    log_level: str = "info"

    # -- Common optional knobs; uncomment when your market needs them.
    # nlp_solver: str | None = None                 # AC stage solver
    # commitment_mode: str = "all_committed"        # or "optimize" / "fixed_initial"
    # commitment_mip_rel_gap: float | None = 1e-3
    # commitment_time_limit_secs: float | None = None
    # voll_per_mwh: float = 9_000.0
    # thermal_overload_cost_per_mwh: float = 5_000.0
    # capture_solver_log: bool = False              # tee Rust tracing + solver console


__all__ = ["Policy"]
