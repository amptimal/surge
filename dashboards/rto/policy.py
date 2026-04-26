# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Solver / formulation policy schema for the RTO dashboard.

Every solve routes through :func:`surge.market.go_c3.solve_workflow`,
so this is in essence a dashboard-friendly view onto
:class:`surge.market.go_c3.MarketPolicy`. The dashboard's HTML form
serializes / deserializes against the field names below; the
``to_market_policy`` translator builds the canonical goc3 policy
object that the native pipeline consumes.

Three knob families:

* **Workflow shape** — ``solve_mode`` (``scuc`` SCUC-only or
  ``scuc_ac_sced`` two-stage) + ``commitment_mode``.
* **Solver tuning** — ``lp_solver`` / ``nlp_solver`` / ``mip_gap`` /
  ``time_limit_secs`` / ``run_pricing``.
* **AC SCED tuning** — ``reactive_support_pin_factor`` plus a
  handful of Ipopt overrides (``sced_ac_opf_tolerance``,
  ``sced_ac_opf_max_iterations``,
  ``disable_sced_thermal_limits``,
  ``ac_relax_committed_pmin_to_zero``).

Loss + security knobs (``loss_mode``, ``loss_rate``,
``loss_max_iterations``, ``security_*``) translate to the
corresponding ``scuc_*`` fields on :class:`MarketPolicy`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


#: Allowed solve modes. ``"scuc"`` runs SCUC alone (no AC reconcile);
#: ``"scuc_ac_sced"`` runs the canonical two-stage SCUC → AC SCED
#: native pipeline. ``"scuc_sced"`` (DC SCED on top of SCUC) is a
#: legacy mode the goc3 native pipeline doesn't expose; we collapse
#: it onto ``"scuc_ac_sced"`` at translation time.
SOLVE_MODES = ("scuc", "scuc_ac_sced")

#: Allowed commitment-source choices for the SCUC stage.
COMMITMENT_MODES = ("optimize", "all_committed", "fixed_initial")

#: Loss-factor cold-start strategies for SCUC's first security iteration.
LOSS_MODES = ("disabled", "uniform", "load_pattern", "dc_pf")


@dataclass(frozen=True)
class RtoPolicy:
    """Dashboard-side solve policy.

    Translated to :class:`surge.market.go_c3.MarketPolicy` at solve
    time via :meth:`to_market_policy`. Field names match the
    dashboard's HTML form so writeForm/readForm round-trip cleanly.
    """

    # Workflow shape
    solve_mode: str = "scuc_ac_sced"
    commitment_mode: str = "optimize"

    # Solver
    lp_solver: str = "highs"
    nlp_solver: str = "ipopt"
    mip_gap: float = 1e-3
    time_limit_secs: float | None = None
    run_pricing: bool = True

    # ── Loss handling ─────────────────────────────────────────────
    loss_mode: str = "disabled"  # see LOSS_MODES
    loss_rate: float = 0.02
    loss_max_iterations: int = 0

    # ── Security / N-1 screening ──────────────────────────────────
    security_enabled: bool = False
    security_max_iterations: int = 10
    security_max_cuts_per_iteration: int = 2_500
    security_preseed_count_per_period: int = 250

    # ── AC SCED tuning ────────────────────────────────────────────
    reactive_support_pin_factor: float = 0.0
    sced_ac_opf_tolerance: float | None = None
    sced_ac_opf_max_iterations: int | None = None
    disable_sced_thermal_limits: bool = False
    ac_relax_committed_pmin_to_zero: bool = False

    # Logging
    log_level: str = "info"

    def __post_init__(self) -> None:
        if self.solve_mode not in SOLVE_MODES:
            raise ValueError(
                f"solve_mode must be one of {SOLVE_MODES!r}, got {self.solve_mode!r}"
            )
        if self.commitment_mode not in COMMITMENT_MODES:
            raise ValueError(
                f"commitment_mode must be one of {COMMITMENT_MODES!r}, got "
                f"{self.commitment_mode!r}"
            )
        if self.loss_mode not in LOSS_MODES:
            raise ValueError(
                f"loss_mode must be one of {LOSS_MODES!r}, got {self.loss_mode!r}"
            )
        if not (0.0 <= self.loss_rate <= 0.5):
            raise ValueError(f"loss_rate must be in [0, 0.5], got {self.loss_rate!r}")
        if self.loss_max_iterations < 0:
            raise ValueError(
                f"loss_max_iterations must be ≥ 0, got {self.loss_max_iterations!r}"
            )
        if self.security_max_iterations < 1:
            raise ValueError(
                f"security_max_iterations must be ≥ 1, got "
                f"{self.security_max_iterations!r}"
            )
        if self.security_max_cuts_per_iteration < 1:
            raise ValueError(
                f"security_max_cuts_per_iteration must be ≥ 1, got "
                f"{self.security_max_cuts_per_iteration!r}"
            )
        if self.security_preseed_count_per_period < 0:
            raise ValueError(
                f"security_preseed_count_per_period must be ≥ 0, got "
                f"{self.security_preseed_count_per_period!r}"
            )
        if self.reactive_support_pin_factor < 0.0:
            raise ValueError(
                f"reactive_support_pin_factor must be ≥ 0, got "
                f"{self.reactive_support_pin_factor!r}"
            )
        if (
            self.sced_ac_opf_tolerance is not None
            and self.sced_ac_opf_tolerance <= 0
        ):
            raise ValueError(
                f"sced_ac_opf_tolerance must be > 0 when set, got "
                f"{self.sced_ac_opf_tolerance!r}"
            )
        if (
            self.sced_ac_opf_max_iterations is not None
            and self.sced_ac_opf_max_iterations < 1
        ):
            raise ValueError(
                f"sced_ac_opf_max_iterations must be ≥ 1 when set, got "
                f"{self.sced_ac_opf_max_iterations!r}"
            )

    def to_market_policy(self) -> Any:
        """Translate to :class:`surge.market.go_c3.MarketPolicy`.

        Maps:

        * ``solve_mode`` ⇒ ``ac_reconcile_mode`` (``"none"`` for
          SCUC-only; ``"ac_dispatch"`` for the two-stage AC SCED
          pipeline).
        * ``commitment_mode`` is forwarded as-is. ``"all_committed"``
          on the goc3 side means every in-service unit is online —
          same semantics.
        * ``loss_mode`` ⇒ ``scuc_loss_factor_warm_start`` tuple (or
          ``None`` for disabled). ``loss_max_iterations`` ⇒
          ``scuc_loss_factor_max_iterations``.
        * ``security_enabled`` controls whether the
          ``scuc_security_*`` knobs ship — when off, we pin
          iterations to 1 and preseed to 0 to keep the security
          screening loop a no-op.
        * AC SCED tuning fields forward 1:1.
        """
        from surge.market.go_c3 import MarketPolicy

        ac_reconcile_mode = "ac_dispatch" if self.solve_mode != "scuc" else "none"

        if self.loss_mode == "disabled":
            scuc_loss_warm_start: tuple[str, float] | None = None
        elif self.loss_mode == "dc_pf":
            scuc_loss_warm_start = ("dc_pf", 0.0)
        else:
            scuc_loss_warm_start = (self.loss_mode, float(self.loss_rate))

        return MarketPolicy(
            formulation="dc",
            ac_reconcile_mode=ac_reconcile_mode,
            commitment_mode=self.commitment_mode,
            lp_solver=self.lp_solver,
            nlp_solver=self.nlp_solver,
            commitment_mip_rel_gap=float(self.mip_gap),
            commitment_time_limit_secs=self.time_limit_secs,
            run_pricing=bool(self.run_pricing),
            scuc_loss_factor_warm_start=scuc_loss_warm_start,
            scuc_loss_factor_max_iterations=int(self.loss_max_iterations),
            scuc_security_max_iterations=(
                int(self.security_max_iterations) if self.security_enabled else 1
            ),
            scuc_security_max_cuts_per_iteration=int(self.security_max_cuts_per_iteration),
            scuc_security_preseed_count_per_period=(
                int(self.security_preseed_count_per_period)
                if self.security_enabled else 0
            ),
            reactive_support_pin_factor=float(self.reactive_support_pin_factor),
            sced_ac_opf_tolerance=self.sced_ac_opf_tolerance,
            sced_ac_opf_max_iterations=self.sced_ac_opf_max_iterations,
            disable_sced_thermal_limits=bool(self.disable_sced_thermal_limits),
            ac_relax_committed_pmin_to_zero=bool(self.ac_relax_committed_pmin_to_zero),
        )


__all__ = [
    "COMMITMENT_MODES",
    "LOSS_MODES",
    "RtoPolicy",
    "SOLVE_MODES",
]
