# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical GO Competition Challenge 3 market adapter.

Thin Python wrapper around the Rust ``surge_market::go_c3`` module
exposed via pyo3. The end-to-end recipe::

    from pathlib import Path
    import surge.market.go_c3 as go_c3

    problem = go_c3.load("scenario_911.json")
    policy = go_c3.MarketPolicy(
        formulation="dc",
        ac_reconcile_mode="ac_dispatch",
        lp_solver="gurobi",
        nlp_solver="ipopt",
    )
    workflow = go_c3.build_workflow(problem, policy)
    result = go_c3.solve_workflow(workflow, lp_solver=policy.lp_solver, nlp_solver=policy.nlp_solver)
    dc = result["stages"][0]["solution"]
    ac = result["stages"][-1]["solution"]
    solution = go_c3.export(problem, ac, dc_reserve_source=dc)
    go_c3.save(solution, "solution.json")

The canonical formulation lives in Rust under
``src/surge-market/src/go_c3/`` and the canonical solver kernel in
``src/surge-dispatch/``.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from surge import _surge as _native


def _uid_index(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    return {row["uid"]: row for row in rows}


@dataclass
class MarketPolicy:
    """Canonical GO C3 market policy.

    Typed wrapper around the policy dict the Rust canonical workflow
    reads. Every field maps directly onto a GoC3Policy field on the
    Rust side or a solver-options knob threaded through the native
    workflow builder.

    Formulation / commitment:
      * ``formulation`` — ``"dc"`` or ``"ac"``. Drives the SCUC stage.
      * ``ac_reconcile_mode`` — ``"ac_dispatch"`` (two-stage SCUC →
        AC SCED) or ``"none"`` (SCUC-only).
      * ``consumer_mode`` — ``"dispatchable"`` (consumer blocks as
        dispatchable loads) or ``"fixed"`` (consumer baseline fixed).
      * ``commitment_mode`` — ``"optimize"``, ``"fixed_initial"``, or
        ``"all_committed"``.
      * ``allow_branch_switching`` — SW1 mode; branch on/off becomes a
        MIP decision.
      * ``commitment_mip_rel_gap`` / ``commitment_time_limit_secs`` —
        MIP optimality gap and solver time limit.

    Solver selection:
      * ``lp_solver`` — ``"gurobi"`` or ``"highs"``.
      * ``nlp_solver`` — AC OPF backend; ``"ipopt"`` or other.

    Logging:
      * ``log_level`` — Python-side logger verbosity.
      * ``capture_solver_log`` — tee Rust tracing + solver console.
    """

    formulation: str = "dc"
    ac_reconcile_mode: str = "ac_dispatch"
    consumer_mode: str = "dispatchable"
    commitment_mode: str = "optimize"
    allow_branch_switching: bool = False
    lp_solver: str = "gurobi"
    nlp_solver: str = "ipopt"
    commitment_mip_rel_gap: float | None = 0.0001
    commitment_time_limit_secs: float | None = 600.0
    # Time-varying MIP gap schedule evaluated as a step function: at
    # wall time ``t`` the acceptable gap is the ``gap`` of the latest
    # pair with ``time_secs <= t``. The solver terminates once the
    # current incumbent is within the target. Pass ``None`` to fall back
    # to the static ``commitment_mip_rel_gap`` +
    # ``commitment_time_limit_secs`` combination. Only Gurobi currently
    # hooks progress callbacks; HiGHS ignores the schedule and falls
    # back to the static safety net.
    #
    # Default: ``None``. The prior front-loaded schedule
    # ((0.0,1e-5),(10.0,1e-4),(30.0,1e-3),(60.0,1e-2),(90.0,1e-1))
    # caused Gurobi to terminate before finding an incumbent on harder
    # GO C3 scenarios (e.g. 73-bus D3/351 went from ~18s to 60s timeout
    # / no incumbent). Keep the knob available for opt-in use.
    commitment_mip_gap_schedule: tuple[tuple[float, float], ...] | None = None
    scuc_thermal_penalty_multiplier: float = 10.0
    sced_thermal_penalty_multiplier: float = 1.0
    scuc_reserve_penalty_multiplier: float = 1.0
    relax_sced_branch_limits_to_dc_slack: bool = False
    sced_branch_relax_margin_mva: float = 0.5
    disable_sced_thermal_limits: bool = False
    sced_bus_balance_safety_multiplier: float = 100.0
    ac_relax_committed_pmin_to_zero: bool = False
    sced_ac_opf_tolerance: float | None = None
    sced_ac_opf_max_iterations: int | None = None
    sced_enforce_regulated_bus_vm_targets: bool = False
    reactive_support_pin_factor: float = 0.0
    # LP repricing re-solve for LMP duals. Off by default on GO C3;
    # the scoring pipeline doesn't consume LMPs and it adds ~15-25s
    # per SCUC on 617-bus.
    run_pricing: bool = False
    # Per-period AC SCED concurrency. None = sequential (default,
    # AC→AC warm-start chained). >=2 = parallel rayon pool of that
    # size; periods drop the AC→AC warm-start, fall back to per-period
    # PF warm-start, and use the bounds-midpoint anchor for ramps.
    # Networks with in-service storage fall back to sequential.
    ac_sced_period_concurrency: int | None = 2
    # Pre-seed iter 0 of the SCUC iterative-screening security loop with
    # this many top-ranked (contingency, monitored) cuts per period.
    # 0 = disabled. Ranking is topology-only (|LODF| scaled by
    # emergency-rating ratio), so the cost is a fraction of one SCUC
    # re-solve. The default (10_000) is larger than any current network's
    # pairs-per-period budget, which effectively seeds every candidate
    # pair — on 73-bus D1 this converges in 1 SCUC iteration and cuts
    # wall time ~50% vs. the baseline iterative-screening loop.
    # Default tuned on 617-D1 case 2: 1000 preseeded cuts/period
    # seeds iter-0 with ~18k structural cuts — enough to cover the
    # top-LODF binding pairs in one shot so the iterative refinement
    # loop converges in 1-2 extra iterations. Higher values bloat the
    # MIP (90k rows at 5000/period couldn't find an incumbent in 60 s
    # on 617-bus); lower values leave many binding pairs for the
    # iterative loop to clean up.
    scuc_security_preseed_count_per_period: int = 250
    # Outer-loop cap for the iterative SCUC N-1 security screening
    # (preseed → solve → check violations → add cuts → repeat). `1`
    # runs a single SCUC solve with only the preseeded cuts.
    scuc_security_max_iterations: int = 5
    # Cap on newly added flowgate cuts per outer iteration. Only
    # active when ``scuc_security_max_iterations > 1``. Sized wide
    # enough to absorb a full violation wave on contingency-dense
    # scenarios where iter 1 surfaces many binding pairs the
    # topology-only preseed missed.
    scuc_security_max_cuts_per_iteration: int = 2_500
    # SCUC loss-factor cold-start warm start for iter 0. Default
    # ("load_pattern", 0.02): PTDF-weighted per-bus loss sensitivity,
    # seeded into the MIP pre-solve. Modes: ``("uniform", rate)``,
    # ``("load_pattern", rate)``, ``("dc_pf", 0.0)``. Pass None to
    # disable. Subsequent security iterations always warm-start from
    # the prior iteration's converged `dloss_dp` regardless of this.
    scuc_loss_factor_warm_start: tuple[str, float] | None = ("load_pattern", 0.02)
    # SCUC loss-factor refinement iteration count. Default 0: trust
    # the warm start, skip the refinement LP. None preserves the
    # historical default of 1 refinement round; 2+ runs further
    # refinement passes.
    scuc_loss_factor_max_iterations: int | None = 0
    # When True, disable flowgate enforcement entirely on the SCUC LP —
    # drops both normal flowgates and the explicit N-1 contingency
    # flowgates. Diagnostic-only; production solves need this False for
    # GO C3 security compliance.
    disable_flowgates: bool = False
    # When True, skip the SCUC MIP warm-start pipeline. The MIP is
    # handed to the solver cold — no load-cover heuristic LP, reduced-
    # relaxed LP, reduced-core MIP, or conservative fallback.
    #
    # Defaulted True: the helpers cost ~9s on 617-bus and the auto
    # short-circuit only trips after the first 1.8s helper already
    # ran. Gurobi solves the SCUC cold within the caller's time budget
    # on our measured cases, so paying the warm-start tax by default
    # isn't worth it. Set False explicitly on scenarios where cold-start
    # doesn't converge.
    disable_scuc_warm_start: bool = True
    # Diagnostic: pin every per-bus power-balance slack column in SCUC
    # to 0 so bus-balance rows are firm. Measures the LP weight of the
    # soft-balance slack family. Off by default.
    scuc_firm_bus_balance_slacks: bool = False
    # Diagnostic: pin every branch thermal slack column in SCUC to 0.
    # Different from ``disable_scuc_thermal_limits`` (which skips the
    # rows entirely); this preserves the rows but removes the slack
    # escape hatch. Off by default.
    scuc_firm_branch_thermal_slacks: bool = False
    # Diagnostic: drop SCUC branch thermal enforcement entirely (skips
    # the row family). Off by default.
    disable_scuc_thermal_limits: bool = False
    log_level: str = "info"
    capture_solver_log: bool = False

    def to_dict(self) -> dict[str, Any]:
        """Policy dict in the shape Rust's `parse_policy` expects.

        Only the Rust-visible fields are returned — Python-only knobs
        like ``log_level`` / ``capture_solver_log`` are consumed by
        the Python runner and stripped here.
        """
        full = asdict(self)
        full.pop("log_level", None)
        full.pop("capture_solver_log", None)
        return full


# Backwards-compat alias — the original class name.
Policy = MarketPolicy


@dataclass
class GoC3Problem:
    """Loaded GO C3 problem with an opaque Rust handle attached.

    The Rust handle is the canonical access path used by the native
    workflow builder. Convenience properties (``periods``,
    ``buses``, ``summary()``, etc.) read the GO C3 JSON dict, which
    is loaded lazily on first access via :meth:`raw`.
    """

    path: Path
    handle: Any  # GoC3Handle
    _raw: dict[str, Any] | None = field(default=None, repr=False, compare=False)

    @classmethod
    def load(cls, path: str | Path) -> GoC3Problem:
        path = Path(path)
        handle = _native.go_c3_load_problem(str(path))
        return cls(path=path, handle=handle)

    @property
    def raw(self) -> dict[str, Any]:
        if self._raw is None:
            with self.path.open("r", encoding="utf-8") as handle:
                self._raw = json.load(handle)
        return self._raw

    @property
    def network(self) -> dict[str, Any]:
        return self.raw["network"]

    @property
    def time_series_input(self) -> dict[str, Any]:
        return self.raw["time_series_input"]

    @property
    def reliability(self) -> dict[str, Any]:
        return self.raw["reliability"]

    @property
    def base_norm_mva(self) -> float:
        return float(self.network["general"]["base_norm_mva"])

    @property
    def periods(self) -> int:
        return int(self.time_series_input["general"]["time_periods"])

    @property
    def interval_durations(self) -> list[float]:
        return [float(value) for value in self.time_series_input["general"]["interval_duration"]]

    @property
    def has_uniform_intervals(self) -> bool:
        values = self.interval_durations
        return all(abs(value - values[0]) <= 1e-9 for value in values)

    @property
    def representative_interval_hours(self) -> float:
        durations = self.interval_durations
        if self.has_uniform_intervals:
            return durations[0]
        return sum(durations) / len(durations)

    @property
    def buses(self) -> list[dict[str, Any]]:
        return self.network.get("bus", [])

    @property
    def buses_by_uid(self) -> dict[str, dict[str, Any]]:
        return _uid_index(self.buses)

    @property
    def shunts(self) -> list[dict[str, Any]]:
        return self.network.get("shunt", [])

    @property
    def devices(self) -> list[dict[str, Any]]:
        return self.network.get("simple_dispatchable_device", [])

    @property
    def devices_by_uid(self) -> dict[str, dict[str, Any]]:
        return _uid_index(self.devices)

    @property
    def device_time_series(self) -> list[dict[str, Any]]:
        return self.time_series_input.get("simple_dispatchable_device", [])

    @property
    def device_time_series_by_uid(self) -> dict[str, dict[str, Any]]:
        return _uid_index(self.device_time_series)

    @property
    def ac_lines(self) -> list[dict[str, Any]]:
        return self.network.get("ac_line", [])

    @property
    def transformers(self) -> list[dict[str, Any]]:
        return self.network.get("two_winding_transformer", [])

    @property
    def dc_lines(self) -> list[dict[str, Any]]:
        return self.network.get("dc_line", [])

    @property
    def violation_costs(self) -> dict[str, Any]:
        return self.network.get("violation_cost", {})

    @property
    def contingencies(self) -> list[dict[str, Any]]:
        return self.reliability.get("contingency", [])

    @property
    def active_reserves(self) -> list[dict[str, Any]]:
        return self.network.get("active_zonal_reserve", [])

    @property
    def active_reserve_time_series(self) -> list[dict[str, Any]]:
        return self.time_series_input.get("active_zonal_reserve", [])

    @property
    def reactive_reserves(self) -> list[dict[str, Any]]:
        return self.network.get("reactive_zonal_reserve", [])

    @property
    def reactive_reserve_time_series(self) -> list[dict[str, Any]]:
        return self.time_series_input.get("reactive_zonal_reserve", [])

    def summary(self) -> dict[str, Any]:
        return {
            "problem_path": str(self.path),
            "periods": self.periods,
            "uniform_intervals": self.has_uniform_intervals,
            "interval_durations": self.interval_durations,
            "base_norm_mva": self.base_norm_mva,
            "counts": {
                "bus": len(self.buses),
                "shunt": len(self.shunts),
                "simple_dispatchable_device": len(self.devices),
                "ac_line": len(self.ac_lines),
                "two_winding_transformer": len(self.transformers),
                "dc_line": len(self.dc_lines),
                "active_zonal_reserve": len(self.active_reserves),
                "reactive_zonal_reserve": len(self.reactive_reserves),
                "contingency": len(self.contingencies),
            },
        }

    # -- MarketProblem protocol -----------------------------------------
    # GO C3 conforms to the markets/ Problem contract: the network and
    # dispatch request are constructed by the Rust adapter, but the
    # entry shape is the same as any other market.

    def build_network(self, policy: Any = None):
        """Build the Surge network for this problem (adapter context attached).

        ``policy`` accepts any object with ``to_dict()`` (e.g.
        :class:`MarketPolicy` or :class:`markets.go_c3.GoC3Policy`) or a
        raw dict. ``None`` uses :class:`MarketPolicy` defaults.
        """
        net, _ = _native.go_c3_build_network(
            self.handle, _policy_to_dict(policy if policy is not None else MarketPolicy())
        )
        return net

    def build_request(self, policy: Any = None) -> dict[str, Any]:
        """Build the canonical :class:`DispatchRequest` for the SCUC stage.

        ``policy`` accepts any object with ``to_dict()`` or a raw dict.
        ``None`` uses :class:`MarketPolicy` defaults.
        """
        return _native.go_c3_build_request(
            self.handle, _policy_to_dict(policy if policy is not None else MarketPolicy())
        )


def load(path: str | Path) -> GoC3Problem:
    """Load a GO C3 problem from disk."""
    return GoC3Problem.load(path)


def _policy_to_dict(policy: MarketPolicy | dict[str, Any]) -> dict[str, Any]:
    if isinstance(policy, dict):
        return policy
    return policy.to_dict()


def build_network(
    problem: GoC3Problem, policy: MarketPolicy | dict[str, Any]
) -> tuple[Any, dict[str, Any]]:
    """Build the Surge network + adapter context from a GO C3 problem."""
    net, ctx = _native.go_c3_build_network(problem.handle, _policy_to_dict(policy))
    return net, ctx


def build_request(
    problem: GoC3Problem, policy: MarketPolicy | dict[str, Any]
) -> dict[str, Any]:
    """Build the typed ``DispatchRequest`` for a GO C3 problem.

    Returns the request as a Python dict (matching the
    ``DispatchRequest`` serde schema). For the standard two-stage
    workflow, prefer :func:`build_workflow` which assembles SCUC + SCED
    stages with proper commitment handoff.
    """
    return _native.go_c3_build_request(problem.handle, _policy_to_dict(policy))


def build_workflow(problem: GoC3Problem, policy: MarketPolicy | dict[str, Any]):
    """Build the canonical two-stage GO C3 market workflow.

    Returns a Rust-native ``NativeMarketWorkflow`` covering DC SCUC →
    AC SCED. The two stages share a prepared :class:`DispatchModel`;
    the executor pins SCED's commitment to SCUC's solved schedule.
    Pass the returned object to :func:`solve_workflow`.
    """
    policy_dict = _policy_to_dict(policy)
    network, _ = _native.go_c3_build_network(problem.handle, policy_dict)
    return _native.go_c3_build_workflow(problem.handle, network, policy_dict)


def solve_workflow(
    workflow,
    *,
    lp_solver: str | None = None,
    nlp_solver: str | None = None,
    stop_after_stage: str | None = None,
) -> dict[str, Any]:
    """Solve a market workflow and return the typed result dict.

    Accepts optional solver-name overrides. When omitted, each stage
    uses its own embedded `DispatchSolveOptions`.

    ``stop_after_stage`` — when set to a stage id (e.g. ``"scuc"``),
    only stages up to and including the named stage are solved. Useful
    for extracting the commitment output without running AC SCED.

    Stage failures are reported in ``result["error"]`` (a dict with
    ``stage_id``, ``role``, ``error``) and ``result["stages"]`` contains
    the successfully solved prior stages. Callers that want the legacy
    "raise on stage error" behavior should check ``result["error"]``
    themselves.
    """
    return _native.solve_market_workflow_py(
        workflow,
        lp_solver=lp_solver,
        nlp_solver=nlp_solver,
        stop_after_stage=stop_after_stage,
    )


def export(
    problem: GoC3Problem,
    dispatch_result,
    *,
    dc_reserve_source=None,
) -> dict[str, Any]:
    """Convert a solved dispatch result back into a GO C3 solution dict.

    When ``dc_reserve_source`` is supplied, active (real-power)
    reserve awards come from that solution while reactive awards stay
    on ``dispatch_result``. Use this when you solved a two-stage
    workflow: pass the AC SCED solution as ``dispatch_result`` and the
    DC SCUC solution as ``dc_reserve_source``.
    """
    return _native.go_c3_export_solution(
        problem.handle, dispatch_result, dc_reserve_source
    )


def save(solution: dict[str, Any], path: str | Path) -> None:
    """Write a GO C3 solution dict to disk as pretty-printed JSON."""
    _native.go_c3_save_solution(solution, str(Path(path)))


__all__ = [
    "GoC3Problem",
    "MarketPolicy",
    "Policy",
    "build_network",
    "build_request",
    "build_workflow",
    "export",
    "load",
    "save",
    "solve_workflow",
]
