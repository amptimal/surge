# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solver policy for the GO Competition Challenge 3 market.

:class:`GoC3Policy` is the user-facing dataclass that callers
instantiate to control a GO C3 solve. Field names match the canonical
Rust policy in :mod:`surge.market.go_c3` — no rename dance, no
per-market adapter dict.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, replace
from typing import Any


DEFAULT_COMMITMENT_MIP_REL_GAP = 1e-3


_PYTHON_ONLY_FIELDS = ("log_level", "capture_solver_log", "scuc_only")


@dataclass(frozen=True)
class GoC3Policy:
    """Knobs for a GO C3 baseline solve.

    Group by intent:

    * **Formulation** — ``formulation``, ``ac_reconcile_mode``,
      ``consumer_mode``, ``commitment_mode``, ``allow_branch_switching``,
      ``scuc_only``.
    * **Solver selection** — ``lp_solver``, ``nlp_solver``.
    * **Commitment MIP tuning** — ``commitment_mip_rel_gap``,
      ``commitment_time_limit_secs``, ``commitment_mip_gap_schedule``,
      ``disable_scuc_warm_start``.
    * **Penalty multipliers** — ``scuc_thermal_penalty_multiplier``,
      ``sced_thermal_penalty_multiplier``,
      ``scuc_reserve_penalty_multiplier``.
    * **SCED tuning** — branch limit relaxation, AC OPF tolerance/
      iterations, bus-balance safety multiplier, reactive pin factor.
    * **Security screening** — preseed cuts, iteration cap, per-iter
      cut cap, flowgate disable switch.
    * **Runtime** — ``ac_sced_period_concurrency``, ``run_pricing``.
    * **Logging** — ``log_level``, ``capture_solver_log``.
    """

    # ── formulation
    formulation: str = "dc"
    ac_reconcile_mode: str = "ac_dispatch"
    consumer_mode: str = "dispatchable"
    commitment_mode: str = "optimize"
    allow_branch_switching: bool = False
    scuc_only: bool = False

    # ── solver selection
    lp_solver: str = "gurobi"
    nlp_solver: str = "ipopt"

    # ── commitment MIP tuning
    commitment_mip_rel_gap: float | None = DEFAULT_COMMITMENT_MIP_REL_GAP
    commitment_time_limit_secs: float | None = None
    commitment_mip_gap_schedule: tuple[tuple[float, float], ...] | None = None
    disable_scuc_warm_start: bool = True

    # ── penalty multipliers
    scuc_thermal_penalty_multiplier: float = 10.0
    sced_thermal_penalty_multiplier: float = 1.0
    scuc_reserve_penalty_multiplier: float = 1.0

    # ── SCED tuning
    relax_sced_branch_limits_to_dc_slack: bool = False
    sced_branch_relax_margin_mva: float = 0.5
    disable_sced_thermal_limits: bool = False
    sced_bus_balance_safety_multiplier: float = 100.0
    ac_relax_committed_pmin_to_zero: bool = False
    sced_ac_opf_tolerance: float | None = None
    sced_ac_opf_max_iterations: int | None = None
    sced_enforce_regulated_bus_vm_targets: bool = False
    reactive_support_pin_factor: float = 0.0

    # ── security screening
    scuc_security_preseed_count_per_period: int = 250
    scuc_security_max_iterations: int = 5
    scuc_security_max_cuts_per_iteration: int = 2_500
    # SCUC loss-factor cold-start warm start. Default
    # ("load_pattern", 0.02): PTDF-weighted per-bus sensitivity seeded
    # into the MIP before the first solve, avoiding a full lossless
    # pass on GO C3. Accepts ("uniform", rate), ("load_pattern", rate),
    # or ("dc_pf", 0.0); pass None to disable. See
    # markets/go_c3/RUNBOOK.md § loss-factor warm start.
    scuc_loss_factor_warm_start: tuple[str, float] | None = ("load_pattern", 0.02)
    # SCUC loss-factor refinement iteration count. Default 0: trust
    # the warm start entirely, skip the refinement LP. Set to 1+ to
    # run refinement rounds on top of the warm start (useful if the
    # warm start is cold/inactive). None preserves the historical
    # GO C3 default of 1 refinement pass.
    scuc_loss_factor_max_iterations: int | None = 0
    disable_flowgates: bool = False
    # Diagnostic: pin every per-bus power-balance slack column in SCUC
    # to 0 (firm bus balance). Measures LP weight of the soft-balance
    # slack family. Off by default.
    scuc_firm_bus_balance_slacks: bool = False
    # Diagnostic: pin every branch thermal slack column in SCUC to 0
    # (firm thermal). Preserves the rows but removes the slack escape
    # hatch. Off by default.
    scuc_firm_branch_thermal_slacks: bool = False
    # Diagnostic: drop SCUC branch thermal enforcement entirely (skips
    # the row family). Off by default.
    disable_scuc_thermal_limits: bool = False
    # Diagnostic: zero out power-balance slack penalty so per-bus
    # balance is free. Combined with disable_scuc_thermal_limits this
    # decouples the network entirely — tests whether UC + reserves
    # alone is solvable. Off by default.
    scuc_copperplate: bool = False
    # Scales the SCUC power-balance penalty (both curtailment and
    # excess segments). Default 1.0 preserves the $1e7/MW curt, $1e5/MW
    # excess ship penalty. Lower values (0.01 / 0.1) make bus slack
    # cheaper — useful on stressed networks where integer commitment
    # forces bus slack usage that the $1e7 penalty blows up into dummy
    # objective values. 0.0 is equivalent to scuc_copperplate.
    scuc_power_balance_penalty_multiplier: float = 1.0

    # When True, drop the SCUC per-bus power-balance row family and its
    # pb_* slack columns from the LP entirely, replacing them with a
    # single system-balance row per period. theta / thermal rows stay
    # but become vestigial (no KCL couples them to pg). DC branch losses
    # are held at the system level via the loss warm-start rate × total
    # period load. The security loop re-solves theta via DC PF before
    # screening so N-1 cuts chase real overloads rather than phantom
    # ones. Default True — speeds up large-network SCUC 10-30x at the
    # cost of relying on security cuts + AC SCED for nodal physics.
    scuc_disable_bus_power_balance: bool = True

    # ── runtime
    ac_sced_period_concurrency: int | None = 2
    run_pricing: bool = False

    # ── logging (Python-only — stripped from the Rust-facing dict)
    log_level: str = "info"
    capture_solver_log: bool = False

    def to_dict(self, *, pin_factor: float | None = None) -> dict[str, Any]:
        """Render as the dict the Rust ``parse_policy`` reads.

        Drops Python-only fields (``log_level``, ``capture_solver_log``,
        ``scuc_only``). Optionally overrides ``reactive_support_pin_factor``.
        """
        d = asdict(self)
        for key in _PYTHON_ONLY_FIELDS:
            d.pop(key, None)
        if pin_factor is not None:
            d["reactive_support_pin_factor"] = pin_factor
        return d

    def with_pin_factor(self, pin_factor: float) -> GoC3Policy:
        """Return a copy with ``reactive_support_pin_factor`` overridden."""
        return replace(self, reactive_support_pin_factor=pin_factor)


__all__ = ["GoC3Policy", "DEFAULT_COMMITMENT_MIP_REL_GAP"]
