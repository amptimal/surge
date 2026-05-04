# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Policy knobs for the datacenter market.

A datacenter solve is always a SCUC: thermal commitment matters
(combustion turbines and fuel cells decide whether to start, and
diesel non-spin sits offline until called), reserves are co-optimised
with energy across the horizon, and the only honest formulation is the
unit-commitment MIP. There is no LP-only "fast" mode — see
:mod:`markets.datacenter.problem` for the asset stack the SCUC drives.

Two orthogonal axes vary across studies:

* :attr:`commitment_mode` — ``"optimize"`` solves the SCUC MIP for
  thermal commitment; ``"fixed"`` pins commitment to an externally
  supplied schedule (replay studies, reference comparisons).
* :attr:`period_coupling` — ``"coupled"`` runs one time-coupled SCUC
  across the whole horizon (storage SOC, thermal min-up/min-down,
  ramps, and 4-CP peak charges link periods); ``"sequential"`` runs
  one SCUC per period with state carried forward (storage SOC and
  commitment), for myopic real-time clearing studies.
"""

from __future__ import annotations

from dataclasses import dataclass


COMMITMENT_MODES = ("optimize", "fixed")
PERIOD_COUPLINGS = ("coupled", "sequential")


@dataclass(frozen=True)
class DataCenterPolicy:
    """Solver / formulation knobs for a datacenter SCUC solve.

    Defaults run a time-coupled SCUC MIP with HiGHS. Switch to Gurobi
    via ``lp_solver="gurobi"`` for larger fleets or tighter MIP gap
    targets.
    """

    #: ``"optimize"`` (default) solves SCUC commitment endogenously.
    #: ``"fixed"`` pins commitment to a caller-supplied schedule —
    #: useful for replaying a reference dispatch or pricing a fixed
    #: schedule.
    commitment_mode: str = "optimize"

    #: ``"coupled"`` (default) runs one SCUC over all periods with
    #: full inter-period coupling. ``"sequential"`` runs N one-period
    #: SCUCs with SOC + commitment carryforward, simulating myopic
    #: real-time clearing.
    period_coupling: str = "coupled"

    #: LP/MIP solver backend.
    lp_solver: str = "highs"

    #: Relative MIP optimality gap for the SCUC commitment optimizer.
    mip_rel_gap: float = 1e-3

    #: Wall-clock cap for the SCUC MIP. Solver returns the best
    #: incumbent if the limit fires.
    mip_time_limit_secs: float = 600.0

    #: When ``True``, every cleared MW of an AS award must be backed
    #: by enough energy headroom (BESS SOC, thermal ramp room) to
    #: deploy at 100% for the whole period. Disabled by default — the
    #: dashboard turns this on once a scenario is past the "explore
    #: roughly" stage.
    enforce_reserve_capacity: bool = False

    #: Logging verbosity — ``"error" | "warn" | "info" | "debug"``.
    log_level: str = "info"

    def __post_init__(self) -> None:
        if self.commitment_mode not in COMMITMENT_MODES:
            raise ValueError(
                f"commitment_mode must be one of {COMMITMENT_MODES!r}, "
                f"got {self.commitment_mode!r}"
            )
        if self.period_coupling not in PERIOD_COUPLINGS:
            raise ValueError(
                f"period_coupling must be one of {PERIOD_COUPLINGS!r}, "
                f"got {self.period_coupling!r}"
            )


__all__ = ["DataCenterPolicy", "COMMITMENT_MODES", "PERIOD_COUPLINGS"]
