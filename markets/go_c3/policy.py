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


_PYTHON_ONLY_FIELDS = (
    "log_level",
    "capture_solver_log",
    "scuc_only",
    "allow_ac_consumer_reserve_shedding",
    "ac_sced_strategy",
)


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
    scuc_thermal_penalty_multiplier: float = 1.25
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
    scuc_security_preseed_count_per_period: int = 0
    scuc_security_max_iterations: int = 10
    scuc_security_max_cuts_per_iteration: int = 250_000
    scuc_security_cut_strategy: str = "adaptive"
    scuc_security_max_active_cuts: int | None = None
    scuc_security_cut_retire_after_rounds: int | None = None
    scuc_security_targeted_cut_threshold: int = 50_000
    scuc_security_targeted_cut_cap: int = 50_000
    # Informational final-pass contingency diagnostic. Disabled by
    # default because large GO C3 networks can produce multi-GB reports.
    scuc_security_near_binding_report: bool = False
    # SCUC loss-factor cold-start warm start. Default
    # ("load_pattern", 0.02): per-bus sensitivity from a synthetic
    # load-pattern DC PF plus sparse adjoint loss solve, seeded into the
    # MIP before the first solve. Accepts ("uniform", rate),
    # ("load_pattern", rate), or ("dc_pf", 0.0); pass None to disable.
    # See markets/go_c3/RUNBOOK.md § loss-factor warm start.
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

    # SCUC system-row loss treatment across security iterations.
    # Only consulted when ``scuc_disable_bus_power_balance=True`` (the
    # canonical GO C3 path). Three modes:
    #
    # * ``"static"`` — single ``rate × total_load`` per period baked
    #   into the system-row RHS. Same value every security iteration.
    #   Cheapest; ignores realized dispatch. Use when you want the
    #   pre-feedback baseline (e.g. for an A/B against the default).
    # * ``"scalar_feedback"`` — after each iter's repaired DC PF,
    #   compute realized losses per period and feed forward as next
    #   iter's RHS. Damped with asymmetric upward bias because
    #   under-commitment costs more than over (AC SCED can't commit
    #   new units to cover a loss-budget miss).
    # * ``"penalty_factors"`` (default) — full marginal-loss-factor
    #   formulation: ``Σ (1 − LF_g) · pg = Σ Pd − L_0`` (linearization-
    #   corrected under distributed-load slack). LFs from realized
    #   flows + loss PTDF, damped, magnitude-capped. Adds the
    #   locational signal so SCUC commits and dispatches with
    #   awareness of where losses concentrate. The earlier 617-bus
    #   A/B (2026-04-25) that placed ``scalar_feedback`` ahead of
    #   ``penalty_factors`` ran before the security-loop fix that
    #   ensures sys-row loss feedback actually lands in a SCUC solve
    #   even when contingency screening converges in one iteration.
    #   With that fix, on 73-bus D1 #303 PF closed the leaderboard
    #   z-gap from $58 k to $15 k (and to ~$2 k when paired with the
    #   winner's commitment).
    #
    # Ignored when ``scuc_disable_bus_power_balance=False`` — the per-
    # bus path's own ``iterate_loss_factors`` machinery handles loss
    # representation directly.
    scuc_loss_treatment: str = "penalty_factors"

    # ── runtime
    ac_sced_period_concurrency: int | None = 2
    ac_sced_strategy: str = "hybrid_retry"
    run_pricing: bool = False

    # ── logging (Python-only — stripped from the Rust-facing dict)
    log_level: str = "info"
    capture_solver_log: bool = False

    # When True, the GO C3 solution exporter caps each consumer's
    # active-reserve awards so the validator's `viol_cs_t_p_on_min` /
    # `viol_cs_t_p_on_max` constraints stay feasible after AC SCED
    # curtails the consumer's served power below the SCUC-awarded
    # reserve level. Up-direction awards (reg_up / syn / ramp_up_on)
    # are capped at `max(0, p_on - p_lb)`; down-direction awards
    # (reg_down / ramp_down_on) at `max(0, p_ub - p_on)`. The shed
    # amount is implicitly accepted as zonal reserve shortfall —
    # preferable to letting the validator stamp the whole solution
    # `feas=0` over a bookkeeping mismatch between the SCUC reserve
    # award and the AC-curtailed dispatch. Default True: the GO C3
    # path's AC SCED has no consumer-side reserve coupling row, so
    # this mismatch is the rule rather than the exception. Set False
    # only for diagnostics that need to expose the raw SCUC awards.
    allow_ac_consumer_reserve_shedding: bool = True

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

    def sections(self) -> dict[str, dict[str, Any]]:
        """Return policy knobs grouped by user-facing intent.

        This is a reporting/interface helper: the dataclass keeps its
        historical flat field names for backward compatibility, while
        dashboards and docs can present the cleaner grouped surface.
        """
        return {
            "formulation": {
                "formulation": self.formulation,
                "ac_reconcile_mode": self.ac_reconcile_mode,
                "consumer_mode": self.consumer_mode,
                "commitment_mode": self.commitment_mode,
                "allow_branch_switching": self.allow_branch_switching,
                "scuc_only": self.scuc_only,
            },
            "solvers": {
                "lp_solver": self.lp_solver,
                "nlp_solver": self.nlp_solver,
            },
            "commitment": {
                "mip_gap": self.commitment_mip_rel_gap,
                "time_limit_s": self.commitment_time_limit_secs,
                "mip_gap_schedule": self.commitment_mip_gap_schedule,
                "warm_start": not self.disable_scuc_warm_start,
            },
            "security": {
                "preseed_count_per_period": self.scuc_security_preseed_count_per_period,
                "max_rounds": self.scuc_security_max_iterations,
                "max_new_cuts_per_round": self.scuc_security_max_cuts_per_iteration,
                "cut_strategy": self.scuc_security_cut_strategy,
                "max_active_cuts": self.scuc_security_max_active_cuts,
                "cut_retire_after_rounds": self.scuc_security_cut_retire_after_rounds,
                "targeted_cut_threshold": self.scuc_security_targeted_cut_threshold,
                "targeted_cut_cap": self.scuc_security_targeted_cut_cap,
                "near_binding_report": self.scuc_security_near_binding_report,
            },
            "losses": {
                "scuc_treatment": self.scuc_loss_treatment,
                "warm_start": self.scuc_loss_factor_warm_start,
                "feedback_rounds": self.scuc_loss_factor_max_iterations,
            },
            "ac_sced": {
                "strategy": self.ac_sced_strategy,
                "parallelism": self.ac_sced_period_concurrency,
                "reactive_support_pin_factor": self.reactive_support_pin_factor,
                "opf_tolerance": self.sced_ac_opf_tolerance,
                "opf_max_iterations": self.sced_ac_opf_max_iterations,
            },
            "reporting": {
                "log_level": self.log_level,
                "capture_solver_log": self.capture_solver_log,
            },
        }

    @classmethod
    def preset(cls, name: str) -> GoC3Policy:
        """Named GO C3 policy presets for common benchmark intents."""
        normalized = name.strip().lower().replace("-", "_")
        if normalized == "fast":
            return cls(
                commitment_mip_rel_gap=5e-3,
                scuc_security_max_iterations=5,
                scuc_security_max_cuts_per_iteration=100_000,
                scuc_security_targeted_cut_cap=20_000,
                scuc_security_cut_retire_after_rounds=2,
                ac_sced_period_concurrency=4,
            )
        if normalized == "balanced":
            return cls()
        if normalized == "strict":
            return cls(
                commitment_mip_rel_gap=5e-4,
                scuc_security_max_iterations=15,
                scuc_security_max_cuts_per_iteration=250_000,
                scuc_security_targeted_cut_cap=100_000,
                ac_sced_period_concurrency=2,
            )
        if normalized == "scale":
            return cls(
                commitment_mip_rel_gap=1e-2,
                scuc_security_max_iterations=10,
                scuc_security_max_cuts_per_iteration=500_000,
                scuc_security_max_active_cuts=1_500_000,
                scuc_security_cut_retire_after_rounds=2,
                scuc_security_targeted_cut_threshold=100_000,
                scuc_security_targeted_cut_cap=50_000,
                ac_sced_period_concurrency=4,
            )
        if normalized == "debug":
            return cls(capture_solver_log=True, log_level="debug")
        raise ValueError(
            "unknown GO C3 policy preset "
            f"{name!r}; expected fast, balanced, strict, scale, or debug"
        )


__all__ = ["GoC3Policy", "DEFAULT_COMMITMENT_MIP_REL_GAP"]
