# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Typed study options and runtimes for the canonical OPF package API."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from enum import Enum
from typing import TYPE_CHECKING

from ._study_inputs import HvdcOpfLink, ParSetpoint, VirtualBid

if TYPE_CHECKING:
    from . import OpfResult
    from .opf import ScopfResult


def _serialize_value(value: object, network: object | None = None) -> object:
    if hasattr(value, "to_native"):
        return value.to_native(network)  # type: ignore[no-any-return]
    if isinstance(value, Mapping):
        return {key: _serialize_value(item, network) for key, item in value.items()}
    if isinstance(value, list):
        return [_serialize_value(item, network) for item in value]
    if isinstance(value, tuple):
        return tuple(_serialize_value(item, network) for item in value)
    return value


def _compact_kwargs(data: Mapping[str, object], network: object | None = None) -> dict[str, object]:
    kwargs: dict[str, object] = {}
    for key, value in data.items():
        if value is None:
            continue
        kwargs[key] = _serialize_value(value, network)
    return kwargs


def _require_positive(name: str, value: float) -> None:
    if value <= 0.0:
        raise ValueError(f"{name} must be positive, got {value}")


def _require_non_negative(name: str, value: float) -> None:
    if value < 0.0:
        raise ValueError(f"{name} must be non-negative, got {value}")


class DcCostModel(str, Enum):
    """Cost formulation for DC-OPF."""

    QUADRATIC = "quadratic"
    """Exact quadratic objective (c2*P^2 + c1*P + c0) solved as QP."""
    PIECEWISE_LINEAR = "piecewise_linear"
    """PWL tangent-line approximation of quadratic costs, solved as pure LP."""


class DcLossModel(str, Enum):
    """Loss modeling approach for DC-OPF."""

    IGNORE = "ignore"
    """Standard lossless DC approximation (default)."""
    ITERATIVE = "iterative"
    """Iteratively adjust penalty factors using marginal loss sensitivities."""


class GeneratorLimitMode(str, Enum):
    """Generator limit enforcement mode for DC-OPF."""

    HARD = "hard"
    """Hard constraints: problem is infeasible if generator limits are violated."""
    SOFT = "soft"
    """Soft constraints: violations are penalized in the objective."""


class DiscreteMode(str, Enum):
    """Discrete control handling for AC-OPF."""

    CONTINUOUS = "continuous"
    """NLP variables remain continuous (default)."""
    ROUND_AND_CHECK = "round_and_check"
    """Round taps/phases/shunts to discrete steps after continuous solve,
    then verify feasibility with AC power flow."""


class HvdcMode(str, Enum):
    """HVDC handling mode for AC-OPF."""

    AUTO = "auto"
    """Auto-detect from network data (default)."""
    ENABLED = "enabled"
    """Force HVDC co-optimization."""
    DISABLED = "disabled"
    """Exclude HVDC from optimization."""


class AcAngleWarmStartMode(str, Enum):
    """Angle initialization strategy for AC-OPF."""

    AUTO = "auto"
    """Use DC-OPF warm start for large cases (n_buses > 2000), DC power flow otherwise."""
    DC_OPF = "dc_opf"
    """Always seed initial angles from a DC-OPF solution."""
    DC_POWER_FLOW = "dc_power_flow"
    """Always seed initial angles from a simple DC power flow."""


class ScopfFormulation(str, Enum):
    """Mathematical formulation for SCOPF."""

    DC = "dc"
    """DC B-theta LP with LODF-based contingency constraints. Fast and scalable."""
    AC = "ac"
    """Full AC NLP with Benders decomposition. Handles voltage and reactive power."""


class ScopfMode(str, Enum):
    """Security enforcement mode for SCOPF."""

    PREVENTIVE = "preventive"
    """Base-case dispatch must satisfy all N-1 constraints (default)."""
    CORRECTIVE = "corrective"
    """Post-contingency corrective redispatch allowed within ramp limits."""


class ThermalRating(str, Enum):
    """Thermal rating tier for post-contingency limits."""

    RATE_A = "rate-a"
    """Long-term continuous rating (branch.rating_a_mva). Default."""
    RATE_B = "rate-b"
    """Short-term emergency rating (branch.rating_b_mva). Falls back to rate-a if zero."""
    RATE_C = "rate-c"
    """Ultimate emergency rating (branch.rating_c_mva). Falls back to rate-a if zero."""


class TransmissionSwitchingFormulation(str, Enum):
    """Formulation for optimal transmission switching (OTS)."""

    DC_MILP = "dc_milp"
    """DC-based mixed-integer linear program with big-M constraints."""
    DC_RELAXED = "dc_relaxed"
    """Continuous relaxation of the DC MILP formulation."""
    DC_ENUMERATE = "dc_enumerate"
    """Enumerate candidate switching actions from DC sensitivity analysis."""


class ReactiveDispatchObjective(str, Enum):
    """Objective function for optimal reactive power dispatch (ORPD)."""

    LOSS = "loss"
    """Minimize total active power losses."""
    VOLTAGE = "voltage"
    """Minimize voltage deviation from a target."""
    COMBINED = "combined"
    """Weighted combination of loss minimization and voltage deviation."""


@dataclass(frozen=True, kw_only=True)
class ConstraintScreening:
    """Active-constraint screening policy for large AC-OPF studies.

    Pre-screens thermal constraints using DC-OPF loading estimates.
    Only branches loaded above the threshold fraction are included in
    the NLP, reducing problem size for large networks.

    Attributes:
        threshold_fraction: Fraction of thermal rating above which a
            branch constraint is included. Default 0.9 (90%).
        minimum_bus_count: Minimum network size to activate screening.
            Smaller networks include all constraints. Default 1000.
        fallback_enabled: When True, run a post-solve violation check
            and re-solve with all constraints if violations are found.
    """

    threshold_fraction: float = 0.9
    minimum_bus_count: int = 1000
    fallback_enabled: bool = False

    def __post_init__(self) -> None:
        if not 0.0 < self.threshold_fraction <= 1.0:
            raise ValueError(
                "ConstraintScreening.threshold_fraction must be in (0, 1], "
                f"got {self.threshold_fraction}"
            )
        if self.minimum_bus_count < 1:
            raise ValueError(
                "ConstraintScreening.minimum_bus_count must be at least 1, "
                f"got {self.minimum_bus_count}"
            )


@dataclass(frozen=True, kw_only=True)
class ScopfScreeningPolicy:
    """Initial contingency screening policy for DC SCOPF.

    Uses LODF-based pre-screening to identify likely-binding
    contingency-branch pairs before entering the constraint generation
    loop.

    Attributes:
        enabled: Whether to pre-screen contingencies. Default True.
        threshold_fraction: LODF loading threshold as fraction of
            thermal rating. Contingencies below this are skipped in the
            initial pass. Default 0.9.
        max_initial_contingencies: Maximum contingencies to seed into
            the initial constraint set. Default 500.
    """

    enabled: bool = True
    threshold_fraction: float = 0.9
    max_initial_contingencies: int = 500

    def __post_init__(self) -> None:
        if not 0.0 < self.threshold_fraction <= 1.0:
            raise ValueError(
                "ScopfScreeningPolicy.threshold_fraction must be in (0, 1], "
                f"got {self.threshold_fraction}"
            )
        if self.max_initial_contingencies < 1:
            raise ValueError(
                "ScopfScreeningPolicy.max_initial_contingencies must be at least 1, "
                f"got {self.max_initial_contingencies}"
            )


@dataclass(frozen=True, kw_only=True)
class DcOpfOptions:
    """Declarative DC-OPF problem options.

    Configures the DC optimal power flow formulation: cost model,
    constraint types, loss approximation, and flowgate enforcement.

    Attributes:
        enforce_thermal_limits: Enforce branch thermal rating constraints.
            Default True.
        minimum_branch_rating_a_mva: Branches with rate_a below this
            value (MVA) are treated as unconstrained. Default 1.0.
        cost_model: Cost formulation. QUADRATIC (default) uses exact
            c2*P^2+c1*P+c0 as a QP. PIECEWISE_LINEAR uses tangent-line
            outer approximation as a pure LP.
        piecewise_linear_breakpoints: Number of tangent lines per
            quadratic generator when cost_model is PIECEWISE_LINEAR.
            More breakpoints improve accuracy. Default 20.
        enforce_flowgates: Include interface and base-case flowgate
            constraints in the LP. Default True.
        par_setpoints: Phase-shifter MW setpoint constraints. Each
            ParSetpoint fixes or bounds a PAR's controlled flow.
        hvdc_links: HVDC links to co-optimize. P_dc becomes a
            decision variable bounded by link capacity.
        generator_limit_mode: HARD (default) makes Pmin/Pmax hard
            constraints. SOFT adds penalty variables when limits are
            violated.
        generator_limit_penalty_per_mw: Penalty cost ($/MW) for
            generator limit violations when mode is SOFT. Required when
            SOFT, forbidden when HARD.
        virtual_bids: Day-ahead virtual energy bids (inc/dec convergence
            bids) for market simulation.
        loss_model: IGNORE (default) for standard lossless DC.
            ITERATIVE adjusts generator penalty factors using marginal
            loss sensitivities pf_i = 1/(1 - dLoss/dP_i).
        loss_iterations: Maximum iterations for loss-factor convergence
            when loss_model is ITERATIVE. Default 3.
        loss_tolerance: Convergence threshold for penalty-factor change
            when loss_model is ITERATIVE. Default 1e-3.
    """

    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0
    cost_model: DcCostModel = DcCostModel.QUADRATIC
    piecewise_linear_breakpoints: int = 20
    enforce_flowgates: bool = True
    par_setpoints: list[ParSetpoint] = field(default_factory=list)
    hvdc_links: list[HvdcOpfLink] = field(default_factory=list)
    generator_limit_mode: GeneratorLimitMode = GeneratorLimitMode.HARD
    generator_limit_penalty_per_mw: float | None = None
    virtual_bids: list[VirtualBid] = field(default_factory=list)
    loss_model: DcLossModel = DcLossModel.IGNORE
    loss_iterations: int = 3
    loss_tolerance: float = 1e-3

    def __post_init__(self) -> None:
        _require_positive(
            "DcOpfOptions.minimum_branch_rating_a_mva", self.minimum_branch_rating_a_mva
        )
        if self.cost_model is DcCostModel.PIECEWISE_LINEAR and self.piecewise_linear_breakpoints < 2:
            raise ValueError(
                "DcOpfOptions.piecewise_linear_breakpoints must be at least 2 when "
                "cost_model=PIECEWISE_LINEAR"
            )
        if self.generator_limit_mode is GeneratorLimitMode.SOFT:
            if self.generator_limit_penalty_per_mw is None:
                raise ValueError(
                    "DcOpfOptions.generator_limit_penalty_per_mw is required when "
                    "generator_limit_mode=SOFT"
                )
            _require_positive(
                "DcOpfOptions.generator_limit_penalty_per_mw",
                self.generator_limit_penalty_per_mw,
            )
        elif self.generator_limit_penalty_per_mw is not None:
            raise ValueError(
                "DcOpfOptions.generator_limit_penalty_per_mw is only valid when "
                "generator_limit_mode=SOFT"
            )
        if self.loss_model is DcLossModel.ITERATIVE:
            if self.loss_iterations < 1:
                raise ValueError(
                    "DcOpfOptions.loss_iterations must be at least 1 when "
                    "loss_model=ITERATIVE"
                )
            _require_positive("DcOpfOptions.loss_tolerance", self.loss_tolerance)

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "enforce_thermal_limits": self.enforce_thermal_limits,
                "min_rate_a": self.minimum_branch_rating_a_mva,
                "use_pwl_costs": self.cost_model is DcCostModel.PIECEWISE_LINEAR,
                "pwl_cost_breakpoints": self.piecewise_linear_breakpoints,
                "enforce_flowgates": self.enforce_flowgates,
                "par_setpoints": self.par_setpoints,
                "hvdc_links": self.hvdc_links or None,
                "gen_limit_penalty": self.generator_limit_penalty_per_mw,
                "virtual_bids": self.virtual_bids,
                "use_loss_factors": self.loss_model is DcLossModel.ITERATIVE,
                "max_loss_iter": self.loss_iterations,
                "loss_tol": self.loss_tolerance,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class DcOpfRuntime:
    """Runtime execution policy for DC-OPF.

    Controls solver backend selection and convergence parameters that
    are independent of the problem formulation.

    Attributes:
        tolerance: LP solver convergence tolerance. Default 1e-8.
        max_iterations: LP solver iteration limit. Default 200.
        lp_solver: Override LP backend. None (default) uses HiGHS.
            Options: "highs", "gurobi", "cplex", "copt".
        warm_start_theta: Optional starting-point bus voltage angles
            (radians) for LP warm-start support.
    """

    tolerance: float = 1e-8
    max_iterations: int = 200
    lp_solver: str | None = None
    warm_start_theta: list[float] | None = None

    def __post_init__(self) -> None:
        _require_positive("DcOpfRuntime.tolerance", self.tolerance)
        if self.max_iterations < 1:
            raise ValueError(
                f"DcOpfRuntime.max_iterations must be at least 1, got {self.max_iterations}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "tolerance": self.tolerance,
                "max_iterations": self.max_iterations,
                "lp_solver": self.lp_solver,
                "warm_start_theta": self.warm_start_theta,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class AcOpfOptions:
    """Declarative AC-OPF problem options.

    Configures the nonlinear AC optimal power flow: constraint types,
    device co-optimization, discrete control handling, and HVDC treatment.

    Attributes:
        enforce_thermal_limits: Enforce branch apparent-power thermal
            limits (S <= rating_a). Default True.
        minimum_branch_rating_a_mva: Branches with rate_a below this
            value (MVA) are treated as unconstrained. Default 1.0.
        enforce_angle_limits: Enforce branch angle-difference limits
            (angmin/angmax). Default False (matches MATPOWER default).
        optimize_switched_shunts: Co-optimize switched shunt susceptance
            as continuous NLP variables. Default False.
        optimize_taps: Co-optimize transformer tap ratios as continuous
            NLP variables within [tap_min, tap_max]. Default False.
        optimize_phase_shifters: Co-optimize phase-shifter angles as
            continuous NLP variables. Default False.
        optimize_svc: Co-optimize SVC/STATCOM susceptance as continuous
            NLP variables. Default False.
        optimize_tcsc: Co-optimize TCSC compensating reactance as
            continuous NLP variables. Default False.
        hvdc_mode: HVDC handling. AUTO (default) detects from network
            data. ENABLED forces HVDC co-optimization. DISABLED excludes
            HVDC from the formulation.
        enforce_capability_curves: Enforce generator P-Q capability
            curves (D-curves) as piecewise-linear NLP constraints.
            Default True.
        discrete_mode: Discrete control handling. CONTINUOUS (default)
            keeps NLP variables continuous. ROUND_AND_CHECK rounds
            taps/phases/shunts to discrete steps after the continuous
            solve and verifies feasibility with AC power flow.
        storage_state_mwh_by_generator_id: Per-generator state-of-charge
            override (MWh) for storage units. Keyed by generator ID.
        interval_hours: Dispatch interval duration (hours) for storage
            SoC bounds. Default 1.0.
        enforce_flowgates: Include interface and flowgate constraints
            from the network model. Default False.
    """

    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0
    enforce_angle_limits: bool = False
    optimize_switched_shunts: bool = False
    optimize_taps: bool = False
    optimize_phase_shifters: bool = False
    optimize_svc: bool = False
    optimize_tcsc: bool = False
    hvdc_mode: HvdcMode = HvdcMode.AUTO
    enforce_capability_curves: bool = True
    discrete_mode: DiscreteMode = DiscreteMode.CONTINUOUS
    storage_state_mwh_by_generator_id: Mapping[str, float] = field(default_factory=dict)
    interval_hours: float = 1.0
    enforce_flowgates: bool = False

    def __post_init__(self) -> None:
        _require_positive(
            "AcOpfOptions.minimum_branch_rating_a_mva", self.minimum_branch_rating_a_mva
        )
        _require_positive("AcOpfOptions.interval_hours", self.interval_hours)
        for generator_id, energy_mwh in self.storage_state_mwh_by_generator_id.items():
            if not isinstance(generator_id, str) or not generator_id:
                raise TypeError(
                    "AcOpfOptions.storage_state_mwh_by_generator_id keys must be non-empty strings"
                )
            _require_non_negative(
                "AcOpfOptions.storage_state_mwh_by_generator_id values", energy_mwh
            )

    def _storage_override_to_indices(self, network: object | None) -> dict[int, float] | None:
        if not self.storage_state_mwh_by_generator_id:
            return None
        if network is None:
            raise TypeError(
                "AcOpfOptions.storage_state_mwh_by_generator_id requires the network to resolve "
                "generator ids"
            )
        return {
            network.generator_index(generator_id): energy_mwh
            for generator_id, energy_mwh in self.storage_state_mwh_by_generator_id.items()
        }

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        include_hvdc: bool | None
        if self.hvdc_mode is HvdcMode.AUTO:
            include_hvdc = None
        else:
            include_hvdc = self.hvdc_mode is HvdcMode.ENABLED
        return _compact_kwargs(
            {
                "enforce_thermal_limits": self.enforce_thermal_limits,
                "min_rate_a": self.minimum_branch_rating_a_mva,
                "enforce_angle_limits": self.enforce_angle_limits,
                "optimize_switched_shunts": self.optimize_switched_shunts,
                "optimize_taps": self.optimize_taps,
                "optimize_phase_shifters": self.optimize_phase_shifters,
                "optimize_svc": self.optimize_svc,
                "optimize_tcsc": self.optimize_tcsc,
                "include_hvdc": include_hvdc,
                "enforce_capability_curves": self.enforce_capability_curves,
                "discrete_mode": self.discrete_mode.value,
                "storage_soc_override": self._storage_override_to_indices(network),
                "dt_hours": self.interval_hours,
                "enforce_flowgates": self.enforce_flowgates,
            },
            network,
        )

    def to_kwargs(self) -> dict[str, object]:
        options = self.to_native_kwargs()
        return {"ac_opf_options": options} if options else {}


@dataclass(frozen=True, kw_only=True)
class AcOpfRuntime:
    """Runtime execution policy for AC-OPF.

    Controls NLP solver backend, convergence, warm-start strategy,
    and constraint screening.

    Attributes:
        tolerance: NLP convergence tolerance. Default 1e-6.
        max_iterations: NLP iteration limit. 0 (default) auto-scales
            based on problem size: max(500, n_buses / 20).
        exact_hessian: Use exact analytical Hessian of the Lagrangian
            (True, default) or L-BFGS quasi-Newton approximation (False).
            Exact Hessian enables superlinear convergence.
        nlp_solver: Override NLP backend. None (default) uses the best
            available runtime backend. Current priority is "copt",
            then "ipopt", then "gurobi".
        print_level: NLP solver verbosity. 0 (default) is silent,
            5 is maximum verbosity.
        warm_start: Prior OPF result to warm-start the NLP solver.
        angle_warm_start: Angle initialization strategy. AUTO (default)
            uses DC-OPF warm start for large cases (n_buses > 2000).
            DC_OPF always seeds from a DC-OPF solution. DC_POWER_FLOW
            always seeds from a simple DC power flow.
        constraint_screening: Active-constraint screening policy for
            large networks. None (default) includes all constraints.
    """

    tolerance: float = 1e-6
    max_iterations: int = 0
    exact_hessian: bool = True
    nlp_solver: str | None = None
    print_level: int = 0
    warm_start: OpfResult | None = None
    angle_warm_start: AcAngleWarmStartMode = AcAngleWarmStartMode.AUTO
    constraint_screening: ConstraintScreening | None = None

    def __post_init__(self) -> None:
        _require_positive("AcOpfRuntime.tolerance", self.tolerance)
        if self.max_iterations < 0:
            raise ValueError(
                f"AcOpfRuntime.max_iterations must be non-negative, got {self.max_iterations}"
            )
        if self.print_level < 0:
            raise ValueError(
                f"AcOpfRuntime.print_level must be non-negative, got {self.print_level}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        if self.angle_warm_start is AcAngleWarmStartMode.AUTO:
            use_dc_opf_warm_start: bool | None = None
        else:
            use_dc_opf_warm_start = self.angle_warm_start is AcAngleWarmStartMode.DC_OPF
        kwargs: dict[str, object] = {
            "tolerance": self.tolerance,
            "max_iterations": self.max_iterations,
            "exact_hessian": self.exact_hessian,
            "nlp_solver": self.nlp_solver,
            "print_level": self.print_level,
            "warm_start": self.warm_start,
            "use_dc_opf_warm_start": use_dc_opf_warm_start,
        }
        if self.constraint_screening is not None:
            kwargs.update(
                {
                    "constraint_screening_threshold": self.constraint_screening.threshold_fraction,
                    "constraint_screening_min_buses": self.constraint_screening.minimum_bus_count,
                    "screening_fallback_enabled": self.constraint_screening.fallback_enabled,
                }
            )
        return _compact_kwargs(kwargs, network)


@dataclass(frozen=True, kw_only=True)
class ScopfOptions:
    """Declarative security-constrained OPF problem options.

    Configures the SCOPF formulation, security mode, contingency
    handling, and constraint types.

    Attributes:
        formulation: DC (default) for linear LODF-based constraints,
            AC for full nonlinear Benders decomposition with voltage
            and reactive constraints.
        mode: PREVENTIVE (default) requires the base-case dispatch to
            satisfy all N-1 constraints. CORRECTIVE allows post-contingency
            redispatch within generator ramp-rate limits.
        corrective_ramp_window_minutes: Time window for corrective
            actions in minutes. Default 10.0 (standard RTO N-1 criterion).
        voltage_threshold_pu: Post-contingency voltage violation
            threshold in per-unit. Default 0.01. AC formulation only.
        contingency_rating: Thermal rating tier for post-contingency
            limits. RATE_A (default), RATE_B (short-term emergency),
            or RATE_C (ultimate emergency).
        enforce_flowgates: Include flowgate and interface constraints.
            Default True.
        enforce_voltage_security: Enforce post-contingency voltage limits
            in AC-SCOPF. Default True.
        max_contingencies: Maximum contingencies to evaluate. 0 (default)
            evaluates all N-1 branch contingencies.
        minimum_branch_rating_a_mva: Branches with rate_a below this
            value are treated as unconstrained. Default 1.0.
        cost_model: DC-SCOPF cost formulation. PIECEWISE_LINEAR
            (default) uses the LP epigraph and avoids HiGHS QP
            numerical issues on large cases. QUADRATIC uses the exact
            quadratic objective on small cases where the QP backend is
            stable.
        dc_opf: Optional DC-OPF sub-options for DC-SCOPF such as PWL
            breakpoint count, loss-factor iteration, and soft
            generator-limit penalties. SCOPF cost selection uses the
            top-level ``cost_model`` field above. Default None.
    """

    formulation: ScopfFormulation = ScopfFormulation.DC
    mode: ScopfMode = ScopfMode.PREVENTIVE
    corrective_ramp_window_minutes: float = 10.0
    voltage_threshold_pu: float = 0.01
    contingency_rating: ThermalRating = ThermalRating.RATE_A
    enforce_flowgates: bool = True
    enforce_voltage_security: bool = True
    max_contingencies: int = 0
    minimum_branch_rating_a_mva: float = 1.0
    enforce_angle_limits: bool = True
    cost_model: DcCostModel = DcCostModel.PIECEWISE_LINEAR
    dc_opf: "DcOpfOptions | None" = None

    def __post_init__(self) -> None:
        _require_positive(
            "ScopfOptions.corrective_ramp_window_minutes", self.corrective_ramp_window_minutes
        )
        _require_positive("ScopfOptions.voltage_threshold_pu", self.voltage_threshold_pu)
        if self.max_contingencies < 0:
            raise ValueError(
                f"ScopfOptions.max_contingencies must be non-negative, got {self.max_contingencies}"
            )
        _require_positive(
            "ScopfOptions.minimum_branch_rating_a_mva", self.minimum_branch_rating_a_mva
        )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        dc = self.dc_opf
        kwargs: dict[str, object] = {
            "formulation": self.formulation.value,
            "mode": self.mode.value,
            "corrective_ramp_window_min": self.corrective_ramp_window_minutes,
            "voltage_threshold": self.voltage_threshold_pu,
            "contingency_rating": self.contingency_rating.value,
            "enforce_flowgates": self.enforce_flowgates,
            "enforce_voltage_security": self.enforce_voltage_security,
            "max_contingencies": self.max_contingencies,
            "min_rate_a": self.minimum_branch_rating_a_mva,
            "enforce_angle_limits": self.enforce_angle_limits,
            # SCOPF uses the top-level cost_model; dc_opf carries the
            # remaining DC sub-options such as breakpoints, losses,
            # and soft-limit penalties.
            "use_pwl_costs": self.cost_model is not DcCostModel.QUADRATIC,
            "pwl_cost_breakpoints": dc.piecewise_linear_breakpoints if dc else 20,
            "gen_limit_penalty": (
                dc.generator_limit_penalty_per_mw
                if dc and dc.generator_limit_mode is GeneratorLimitMode.SOFT
                else None
            ),
            "use_loss_factors": dc.loss_model is DcLossModel.ITERATIVE if dc else False,
            "max_loss_iter": dc.loss_iterations if dc else 3,
            "loss_tol": dc.loss_tolerance if dc else 1e-3,
            "enforce_thermal_limits": dc.enforce_thermal_limits if dc else True,
            "par_setpoints": dc.par_setpoints if dc and dc.par_setpoints else None,
            "hvdc_links": dc.hvdc_links if dc and dc.hvdc_links else None,
        }
        return _compact_kwargs(kwargs, network)


@dataclass(frozen=True, kw_only=True)
class ScopfRuntime:
    """Runtime execution policy for SCOPF.

    Controls constraint generation convergence, solver backends, and
    pre-screening behavior.

    Attributes:
        violation_tolerance_pu: Post-contingency flow violation threshold
            in per-unit. Constraint generation terminates when no
            violations exceed this value. Default 0.01 (= 1 MW at
            100 MVA base).
        max_iterations: Maximum constraint generation iterations.
            Default 20.
        max_cuts_per_iteration: Maximum violated contingency-branch
            pairs added per iteration. Default 100.
        lp_solver: LP backend for DC-SCOPF. None (default) uses HiGHS.
            Options: "highs", "gurobi", "cplex", "copt".
        nlp_solver: NLP backend for AC-SCOPF. None (default) uses the
            best available runtime backend. Current priority is "copt",
            then "ipopt", then "gurobi".
        newton_max_iterations: Maximum NR iterations for AC
            post-contingency subproblem solves. Default 30.
        newton_tolerance: NR convergence tolerance for AC
            post-contingency subproblems. Default 1e-6.
        screening: LODF-based pre-screening policy for DC SCOPF.
            Pre-populates the initial constraint set with likely-binding
            contingency-branch pairs.
        warm_start: Prior SCOPF result to warm-start the constraint
            generation loop.
    """

    violation_tolerance_pu: float = 0.01
    max_iterations: int = 20
    max_cuts_per_iteration: int = 100
    lp_solver: str | None = None
    nlp_solver: str | None = None
    newton_max_iterations: int = 30
    newton_tolerance: float = 1e-6
    screening: ScopfScreeningPolicy = field(default_factory=ScopfScreeningPolicy)
    warm_start: ScopfResult | None = None

    def __post_init__(self) -> None:
        _require_positive("ScopfRuntime.violation_tolerance_pu", self.violation_tolerance_pu)
        if self.max_iterations < 1:
            raise ValueError(
                f"ScopfRuntime.max_iterations must be at least 1, got {self.max_iterations}"
            )
        if self.max_cuts_per_iteration < 1:
            raise ValueError(
                "ScopfRuntime.max_cuts_per_iteration must be at least 1, "
                f"got {self.max_cuts_per_iteration}"
            )
        if self.newton_max_iterations < 1:
            raise ValueError(
                "ScopfRuntime.newton_max_iterations must be at least 1, "
                f"got {self.newton_max_iterations}"
            )
        _require_positive("ScopfRuntime.newton_tolerance", self.newton_tolerance)

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        kwargs = {
            "tolerance": self.violation_tolerance_pu,
            "max_iterations": self.max_iterations,
            "max_cuts_per_iteration": self.max_cuts_per_iteration,
            "lp_solver": self.lp_solver,
            "nlp_solver": self.nlp_solver,
            "nr_max_iterations": self.newton_max_iterations,
            "nr_convergence_tolerance": self.newton_tolerance,
            "warm_start": self.warm_start,
        }
        if self.screening.enabled:
            kwargs.update(
                {
                    "enable_screener": True,
                    "screener_threshold_fraction": self.screening.threshold_fraction,
                    "screener_max_initial_contingencies": self.screening.max_initial_contingencies,
                }
            )
        else:
            kwargs["enable_screener"] = False
        return _compact_kwargs(kwargs, network)


@dataclass(frozen=True, kw_only=True)
class TransmissionSwitchingOptions:
    """Declarative optimal transmission switching problem options.

    Configures which branches are switchable and the optimization
    formulation for topology optimization.

    Attributes:
        formulation: OTS formulation. DC_MILP (default) uses big-M
            constraints. DC_RELAXED uses continuous relaxation.
            DC_ENUMERATE enumerates candidates from DC sensitivities.
        maximum_open_switches: Maximum number of branches that may be
            opened simultaneously. None (default) is unconstrained.
        switchable_branch_indices: Explicit list of branch indices
            eligible for switching. Mutually exclusive with
            switchable_rating_threshold_mva.
        switchable_rating_threshold_mva: All branches with rate_a
            above this threshold are switchable. Mutually exclusive with
            switchable_branch_indices.
        big_m: Big-M constant for MILP formulation. None uses an
            automatic value based on network characteristics.
    """

    formulation: TransmissionSwitchingFormulation = TransmissionSwitchingFormulation.DC_MILP
    maximum_open_switches: int | None = None
    switchable_branch_indices: list[int] | None = None
    switchable_rating_threshold_mva: float | None = None
    big_m: float | None = None

    def __post_init__(self) -> None:
        if (
            self.switchable_branch_indices is not None
            and self.switchable_rating_threshold_mva is not None
        ):
            raise ValueError(
                "TransmissionSwitchingOptions accepts either switchable_branch_indices or "
                "switchable_rating_threshold_mva, not both"
            )
        if self.maximum_open_switches is not None and self.maximum_open_switches < 0:
            raise ValueError(
                "TransmissionSwitchingOptions.maximum_open_switches must be non-negative when set"
            )
        if self.switchable_rating_threshold_mva is not None:
            _require_positive(
                "TransmissionSwitchingOptions.switchable_rating_threshold_mva",
                self.switchable_rating_threshold_mva,
            )
        if self.big_m is not None:
            _require_positive("TransmissionSwitchingOptions.big_m", self.big_m)

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "formulation": self.formulation.value,
                "max_switches_open": self.maximum_open_switches,
                "switchable_branch_indices": self.switchable_branch_indices,
                "switchable_rating_threshold": self.switchable_rating_threshold_mva,
                "big_m": self.big_m,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class TransmissionSwitchingRuntime:
    """Runtime execution policy for transmission switching studies.

    Attributes:
        tolerance: Solver convergence tolerance. Default 1e-6.
        lp_solver: Override LP/MIP backend. None (default) uses HiGHS.
            Options: "highs", "gurobi", "cplex", "copt".
        time_limit_secs: MIP solver time limit in seconds. Default 300.
        mip_gap: MIP optimality gap tolerance. Default 0.01 (1%).
        max_iterations: Solver iteration limit. Default 1000.
    """

    tolerance: float = 1e-6
    lp_solver: str | None = None
    time_limit_secs: float = 300.0
    mip_gap: float = 0.01
    max_iterations: int = 1000

    def __post_init__(self) -> None:
        _require_positive("TransmissionSwitchingRuntime.tolerance", self.tolerance)
        _require_positive("TransmissionSwitchingRuntime.time_limit_secs", self.time_limit_secs)
        _require_non_negative("TransmissionSwitchingRuntime.mip_gap", self.mip_gap)
        if self.max_iterations < 1:
            raise ValueError(
                "TransmissionSwitchingRuntime.max_iterations must be at least 1, "
                f"got {self.max_iterations}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "tolerance": self.tolerance,
                "lp_solver": self.lp_solver,
                "time_limit_secs": self.time_limit_secs,
                "mip_gap": self.mip_gap,
                "max_iterations": self.max_iterations,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class ReactiveDispatchOptions:
    """Declarative reactive dispatch / volt-var optimization problem options.

    Configures the ORPD objective function, device optimization, and
    constraint handling.

    Attributes:
        objective: Optimization objective. LOSS minimizes active losses.
            VOLTAGE minimizes deviation from voltage_target_pu. COMBINED
            uses a weighted sum of both.
        voltage_target_pu: Voltage target for voltage and combined
            objectives. Default 1.0 p.u.
        loss_weight: Weight on active losses in the combined objective.
            Default 1.0.
        voltage_weight: Weight on voltage deviation in the combined
            objective. Default 1.0.
        fix_active_power: Keep generator active power fixed at current
            dispatch. Default True (optimize reactive only).
        optimize_reactive_power: Allow reactive power setpoints to
            vary. Default True.
        enforce_thermal_limits: Enforce branch thermal constraints.
            Default True.
        minimum_branch_rating_a_mva: Branches with rate_a below this
            value are unconstrained. Default 1.0.
    """

    objective: ReactiveDispatchObjective = ReactiveDispatchObjective.LOSS
    voltage_target_pu: float = 1.0
    loss_weight: float = 1.0
    voltage_weight: float = 1.0
    fix_active_power: bool = True
    optimize_reactive_power: bool = True
    enforce_thermal_limits: bool = True
    minimum_branch_rating_a_mva: float = 1.0

    def __post_init__(self) -> None:
        _require_positive("ReactiveDispatchOptions.voltage_target_pu", self.voltage_target_pu)
        _require_non_negative("ReactiveDispatchOptions.loss_weight", self.loss_weight)
        _require_non_negative("ReactiveDispatchOptions.voltage_weight", self.voltage_weight)
        _require_positive(
            "ReactiveDispatchOptions.minimum_branch_rating_a_mva",
            self.minimum_branch_rating_a_mva,
        )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "objective": self.objective.value,
                "v_ref": self.voltage_target_pu,
                "loss_weight": self.loss_weight,
                "voltage_weight": self.voltage_weight,
                "fix_pg": self.fix_active_power,
                "optimize_q": self.optimize_reactive_power,
                "enforce_thermal_limits": self.enforce_thermal_limits,
                "min_rate_a": self.minimum_branch_rating_a_mva,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class ReactiveDispatchRuntime:
    """Runtime execution policy for reactive dispatch studies.

    Attributes:
        tolerance: NLP convergence tolerance. Default 1e-6.
        max_iterations: NLP iteration limit. 0 (default) auto-scales.
        exact_hessian: Use exact analytical Hessian (True, default)
            or L-BFGS approximation (False).
        nlp_solver: Override NLP backend. None (default) uses the best
            available runtime backend. Current priority is "copt",
            then "ipopt", then "gurobi".
        print_level: NLP solver verbosity (0=silent, 5=verbose).
    """

    tolerance: float = 1e-6
    max_iterations: int = 0
    exact_hessian: bool = True
    nlp_solver: str | None = None
    print_level: int = 0

    def __post_init__(self) -> None:
        _require_positive("ReactiveDispatchRuntime.tolerance", self.tolerance)
        if self.max_iterations < 0:
            raise ValueError(
                "ReactiveDispatchRuntime.max_iterations must be non-negative, "
                f"got {self.max_iterations}"
            )
        if self.print_level < 0:
            raise ValueError(
                f"ReactiveDispatchRuntime.print_level must be non-negative, got {self.print_level}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {
                "tolerance": self.tolerance,
                "max_iter": self.max_iterations,
                "exact_hessian": self.exact_hessian,
                "nlp_solver": self.nlp_solver,
                "print_level": self.print_level,
            },
            network,
        )


@dataclass(frozen=True, kw_only=True)
class ReconfigurationOptions:
    """Declarative network reconfiguration problem options.

    Attributes:
        max_open_branches: Maximum number of branches that may be
            opened in the reconfiguration. Default 1.
    """

    max_open_branches: int = 1

    def __post_init__(self) -> None:
        if self.max_open_branches < 0:
            raise ValueError(
                f"ReconfigurationOptions.max_open_branches must be non-negative, got {self.max_open_branches}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs({"max_switches": self.max_open_branches}, network)


@dataclass(frozen=True, kw_only=True)
class ReconfigurationRuntime:
    """Runtime execution policy for MISOCP network reconfiguration.

    Attributes:
        tolerance: Solver convergence tolerance. Default 1e-6.
        max_iterations: Solver iteration limit. Default 200.
    """

    tolerance: float = 1e-6
    max_iterations: int = 200

    def __post_init__(self) -> None:
        _require_positive("ReconfigurationRuntime.tolerance", self.tolerance)
        if self.max_iterations < 1:
            raise ValueError(
                f"ReconfigurationRuntime.max_iterations must be at least 1, got {self.max_iterations}"
            )

    def to_native_kwargs(self, network: object | None = None) -> dict[str, object]:
        return _compact_kwargs(
            {"tolerance": self.tolerance, "max_iterations": self.max_iterations},
            network,
        )


__all__ = [
    "AcAngleWarmStartMode",
    "AcOpfRuntime",
    "AcOpfOptions",
    "ConstraintScreening",
    "DcCostModel",
    "DcLossModel",
    "DcOpfRuntime",
    "DcOpfOptions",
    "DiscreteMode",
    "GeneratorLimitMode",
    "HvdcMode",
    "ReactiveDispatchObjective",
    "ReactiveDispatchRuntime",
    "ReactiveDispatchOptions",
    "ReconfigurationRuntime",
    "ReconfigurationOptions",
    "ScopfFormulation",
    "ScopfMode",
    "ScopfRuntime",
    "ScopfScreeningPolicy",
    "ScopfOptions",
    "ThermalRating",
    "TransmissionSwitchingFormulation",
    "TransmissionSwitchingRuntime",
    "TransmissionSwitchingOptions",
]
