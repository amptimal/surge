# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Market configuration: penalties, network rules, and AC reconciliation defaults.

:meth:`MarketConfig.default` is the canonical starting point for a new market;
the defaults come from the GO Competition Challenge 3 recipe but are generic
enough for most dispatch studies. Callers can override individual fields or
swap to another named preset.
"""

from __future__ import annotations

import copy
import dataclasses
from dataclasses import dataclass, field
from typing import Any


# ---------------------------------------------------------------------------
# Penalty curves
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class PenaltyCurveSegment:
    """One segment of a piecewise-linear penalty curve."""

    max_violation: float
    cost_per_unit: float

    def to_dict(self) -> dict[str, Any]:
        return {"max_violation": self.max_violation, "cost_per_unit": self.cost_per_unit}


@dataclass(frozen=True)
class PenaltyCurve:
    """Penalty cost function for a soft constraint.

    ``type`` is ``"linear"`` (single slope) or ``"piecewise_linear"``
    (multiple segments with increasing cost).
    """

    type: str
    cost_per_unit: float | None = None
    segments: tuple[PenaltyCurveSegment, ...] | None = None

    def to_dict(self) -> dict[str, Any]:
        if self.type == "piecewise_linear" and self.segments:
            return {
                "type": "piecewise_linear",
                "segments": [s.to_dict() for s in self.segments],
            }
        return {"type": "linear", "cost_per_unit": self.cost_per_unit or 0.0}


# ---------------------------------------------------------------------------
# Penalty configuration
# ---------------------------------------------------------------------------

@dataclass
class PenaltyConfig:
    """Penalty costs for all soft constraints in the dispatch formulation.

    Costs are in $/MW (or $/MVA, $/MVar, $/rad as appropriate).
    The Surge solver internally multiplies by ``base_mva`` as needed.
    """

    thermal: PenaltyCurve
    voltage_high: PenaltyCurve
    voltage_low: PenaltyCurve
    power_balance: PenaltyCurve
    ramp: PenaltyCurve
    angle: PenaltyCurve
    reserve: PenaltyCurve

    def to_dict(self) -> dict[str, Any]:
        return {
            "thermal": self.thermal.to_dict(),
            "voltage_high": self.voltage_high.to_dict(),
            "voltage_low": self.voltage_low.to_dict(),
            "power_balance": self.power_balance.to_dict(),
            "ramp": self.ramp.to_dict(),
            "angle": self.angle.to_dict(),
            "reserve": self.reserve.to_dict(),
        }


# ---------------------------------------------------------------------------
# Network enforcement rules
# ---------------------------------------------------------------------------

@dataclass
class ThermalLimitRules:
    enforce: bool = True
    min_rate_a: float = 1.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "enforce": self.enforce,
            "min_rate_a": self.min_rate_a,
        }


@dataclass
class FlowgateRules:
    enabled: bool = False
    max_nomogram_iterations: int = 10

    def to_dict(self) -> dict[str, Any]:
        return {
            "enabled": self.enabled,
            "max_nomogram_iterations": self.max_nomogram_iterations,
        }


@dataclass
class LossFactorRules:
    enabled: bool = True
    max_iterations: int = 1
    tolerance: float = 1e-3

    def to_dict(self) -> dict[str, Any]:
        return {
            "enabled": self.enabled,
            "max_iterations": self.max_iterations,
            "tolerance": self.tolerance,
        }


@dataclass
class CommitmentTransitionRules:
    shutdown_deloading: bool = True
    trajectory_mode: str = "offline_trajectory"

    def to_dict(self) -> dict[str, Any]:
        return {
            "shutdown_deloading": self.shutdown_deloading,
            "trajectory_mode": self.trajectory_mode,
        }


@dataclass
class RampRules:
    mode: str = "averaged"
    enforcement: str = "hard"

    def to_dict(self) -> dict[str, Any]:
        return {
            "mode": self.mode,
            "enforcement": self.enforcement,
        }


@dataclass
class EnergyWindowRules:
    enforcement: str = "soft"
    penalty_per_puh: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "enforcement": self.enforcement,
            "penalty_per_puh": self.penalty_per_puh,
        }


@dataclass
class TopologyControlRules:
    mode: str = "fixed"
    branch_switching_big_m_factor: float = 10.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "mode": self.mode,
            "branch_switching_big_m_factor": self.branch_switching_big_m_factor,
        }


@dataclass
class NetworkRules:
    """Network policy families for the canonical dispatch request surface."""

    thermal_limits: ThermalLimitRules = field(default_factory=ThermalLimitRules)
    flowgates: FlowgateRules = field(default_factory=FlowgateRules)
    loss_factors: LossFactorRules = field(default_factory=LossFactorRules)
    commitment_transitions: CommitmentTransitionRules = field(default_factory=CommitmentTransitionRules)
    ramping: RampRules = field(default_factory=RampRules)
    energy_windows: EnergyWindowRules = field(default_factory=EnergyWindowRules)
    topology_control: TopologyControlRules = field(default_factory=TopologyControlRules)

    def to_dict(self) -> dict[str, Any]:
        return {
            "thermal_limits": self.thermal_limits.to_dict(),
            "flowgates": self.flowgates.to_dict(),
            "loss_factors": self.loss_factors.to_dict(),
            "commitment_transitions": self.commitment_transitions.to_dict(),
            "ramping": self.ramping.to_dict(),
            "energy_windows": self.energy_windows.to_dict(),
            "topology_control": self.topology_control.to_dict(),
        }


# ---------------------------------------------------------------------------
# AC reconciliation configuration
# ---------------------------------------------------------------------------

@dataclass
class AcReconcileConfig:
    """Configuration for the AC OPF reconciliation pass.

    OPF override fields match the Surge ``AcOpfOptions`` / ``AcOpfRuntime``
    names.  Target-tracking fields control the quadratic penalty that steers
    the AC solution toward the DC dispatch.
    """

    # OPF overrides
    thermal_limit_slack_penalty_per_mva: float = 5.0
    bus_active_power_balance_slack_penalty_per_mw: float = 1_000_000.0
    bus_reactive_power_balance_slack_penalty_per_mvar: float = 1_000_000.0
    max_iterations: int = 3_000
    exact_hessian: bool = True
    enforce_regulated_bus_vm_targets: bool = False
    # Tap and phase-shifter optimization are **off by default**. The
    # NLP landscape in tap/phase dimensions is very flat (dual-degenerate
    # on 617-bus+ and even 73-bus scenarios with nontrivial tap counts),
    # which makes Ipopt oscillate or hit its iteration cap without
    # finding a descent direction. See the `project_go_c3_tap_degeneracy`
    # memory for the full investigation. Taps stay at their initial
    # values (seeded from GO C3 problem data in the adapter) and any
    # resulting voltage/flow deviations are priced through the GO
    # validator's soft penalties. Switched shunts are kept on — they
    # converge reliably in the NLP and the discrete rounding step closes
    # the small continuous→discrete gap.
    optimize_taps: bool = False
    optimize_phase_shifters: bool = False
    optimize_switched_shunts: bool = True
    discrete_mode: str = "RoundAndCheck"

    # Target tracking
    #
    # With distributed banding (below), the AC OPF is free to redispatch
    # against its real offer curves rather than being held to the DC LP's
    # pick by a second cost term. The canonical GO C3 path now leaves the
    # generator trust-region penalty off by default; the band limits are
    # enough to keep the AC polish numerically well behaved on the cases we
    # care about, and a nonzero quadratic pullback was leaving served-load
    # value on the table on 73-bus cases like D1/357. Populate per-gen
    # overrides from LP duals via the TT-2b path in `go_c3.runner` if we
    # want an asymmetric, economics-aware trust region later.
    generator_p_penalty_per_mw2: float = 0.0
    dispatchable_load_p_penalty_per_mw2: float = 0.0

    # Distributed dispatch pinning
    #
    # The bandable producer subset (slack-bus gens + top-N non-slack by
    # Q range, see `_select_bandable_producer_resource_ids`) gets a
    # symmetric P band of
    #   min(producer_band_cap_mw,
    #       max(producer_band_floor_mw, |target| * producer_band_fraction))
    # around its DC dispatch target, clipped to the physical per-period
    # envelope the adapter wrote into `generator_dispatch_bounds`. Every
    # non-bandable producer stays hard-pinned at its DC target.
    #
    # Default `max_additional_bandable_producers = 0` (in the pin helper)
    # keeps the bandable set to slack-bus gens only — pristine behavior.
    # The `producer_band_fraction`/`floor`/`cap` values tune the slack-
    # bus band; `cap = 1e9` effectively disables the cap so the physical
    # per-period envelope is what clips. Widening to >0 additional gens
    # is blocked on resolving an Ipopt NLP fragility on 73-bus D2 911
    # period 15 near bus 34 — see the Stage 1 debug session for details.
    producer_band_fraction: float = 0.05
    producer_band_floor_mw: float = 1.0
    producer_band_cap_mw: float = 1.0e9

    def to_opf_overrides_dict(self) -> dict[str, Any]:
        """Build the ``runtime.ac_opf`` dict for a dispatch request."""
        return {
            "thermal_limit_slack_penalty_per_mva": self.thermal_limit_slack_penalty_per_mva,
            "bus_active_power_balance_slack_penalty_per_mw": self.bus_active_power_balance_slack_penalty_per_mw,
            "bus_reactive_power_balance_slack_penalty_per_mvar": self.bus_reactive_power_balance_slack_penalty_per_mvar,
            "max_iterations": self.max_iterations,
            "exact_hessian": self.exact_hessian,
            "enforce_regulated_bus_vm_targets": self.enforce_regulated_bus_vm_targets,
            "optimize_taps": self.optimize_taps,
            "optimize_phase_shifters": self.optimize_phase_shifters,
            "optimize_switched_shunts": self.optimize_switched_shunts,
            "discrete_mode": self.discrete_mode,
        }

    def to_target_tracking_dict(self) -> dict[str, Any]:
        """Build the ``runtime.ac_target_tracking`` dict for a dispatch request."""
        return {
            "generator_p_penalty_per_mw2": self.generator_p_penalty_per_mw2,
            "dispatchable_load_p_penalty_per_mw2": self.dispatchable_load_p_penalty_per_mw2,
        }


# ---------------------------------------------------------------------------
# Benders decomposition configuration
# ---------------------------------------------------------------------------

@dataclass
class BendersConfig:
    """Configuration for SCED-AC Benders decomposition."""

    max_iterations: int = 10
    rel_tol: float = 1e-4
    abs_tol: float = 1.0
    min_slack_dollars_per_hour: float = 1e-3
    marginal_trim_dollars_per_mw_per_hour: float = 1e-6
    trust_region_expansion_factor: float = 2.0
    trust_region_contraction_factor: float = 0.5
    trust_region_min_mw: float = 1.0
    cut_dedup_marginal_tol: float = 1e-9
    stagnation_patience: int = 3
    oscillation_patience: int = 4
    ac_opf_thermal_slack_penalty_per_mva: float = 1.0e4
    # Bus balance penalties default to the violation costs × safety factor.
    # Set to None to auto-derive from violation costs.
    ac_opf_bus_active_power_balance_slack_penalty_per_mw: float | None = None
    ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar: float | None = None

    def to_orchestration_dict(
        self,
        p_bus_penalty_fallback: float = 50_000.0,
        q_bus_penalty_fallback: float = 50_000.0,
    ) -> dict[str, Any]:
        d: dict[str, Any] = {}
        for f in dataclasses.fields(self):
            val = getattr(self, f.name)
            if val is not None:
                d[f.name] = val
        d.setdefault(
            "ac_opf_bus_active_power_balance_slack_penalty_per_mw",
            p_bus_penalty_fallback,
        )
        d.setdefault(
            "ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar",
            q_bus_penalty_fallback,
        )
        return d


# ---------------------------------------------------------------------------
# Top-level market configuration
# ---------------------------------------------------------------------------

@dataclass
class MarketConfig:
    """Complete market configuration for a dispatch study.

    Use ``MarketConfig.default()`` for the canonical starting preset, then
    override individual fields or swap to another named preset.
    """

    penalties: PenaltyConfig
    network_rules: NetworkRules = field(default_factory=NetworkRules)
    ac_reconcile: AcReconcileConfig = field(default_factory=AcReconcileConfig)
    benders: BendersConfig = field(default_factory=BendersConfig)

    @classmethod
    def default(
        cls,
        base_mva: float = 100.0,
        *,
        s_vio_cost: float = 500.0,
        p_bus_vio_cost: float = 1_000_000.0,
        q_bus_vio_cost: float | None = None,
        e_vio_cost: float = 0.0,
        max_bid_cost: float = 0.0,
    ) -> MarketConfig:
        """Create the canonical default market configuration.

        The numbers come from the GO Competition Challenge 3 reference
        recipe (thermal, voltage, bus-balance, ramp, angle, reserve
        penalties) but are generic enough to be the neutral starting
        point for any dispatch study. Override individual fields or
        swap in a different preset when a market needs a bespoke
        penalty tensor.

        Args:
            base_mva: System MVA base (used to scale penalties).
            s_vio_cost: Branch thermal violation cost ($/pu/hr).
            p_bus_vio_cost: Active power bus balance violation cost ($/pu/hr).
            q_bus_vio_cost: Reactive power bus balance violation cost ($/pu/hr).
                Defaults to ``p_bus_vio_cost`` if not provided.
            e_vio_cost: Energy window violation cost ($/pu/hr).
            max_bid_cost: Maximum generator bid cost ($/MWh), used to scale
                the ramp violation penalty.
        """
        if q_bus_vio_cost is None:
            q_bus_vio_cost = p_bus_vio_cost

        s_vio = float(s_vio_cost) / base_mva
        p_bus = float(p_bus_vio_cost) / base_mva
        ramp_cost = max(max_bid_cost * 10.0, 1_000_000.0 / base_mva)

        piecewise_voltage = PenaltyCurve(
            type="piecewise_linear",
            segments=(
                PenaltyCurveSegment(max_violation=0.01, cost_per_unit=5_000.0 / base_mva),
                PenaltyCurveSegment(max_violation=1.0e30, cost_per_unit=50_000.0 / base_mva),
            ),
        )

        penalties = PenaltyConfig(
            thermal=PenaltyCurve(type="linear", cost_per_unit=s_vio),
            voltage_high=piecewise_voltage,
            voltage_low=piecewise_voltage,
            power_balance=PenaltyCurve(type="linear", cost_per_unit=p_bus),
            ramp=PenaltyCurve(type="linear", cost_per_unit=ramp_cost),
            angle=PenaltyCurve(type="linear", cost_per_unit=500.0 / base_mva),
            reserve=PenaltyCurve(type="linear", cost_per_unit=1_000.0 / base_mva),
        )

        network_rules = NetworkRules(
            energy_windows=EnergyWindowRules(
                penalty_per_puh=float(e_vio_cost) / base_mva if e_vio_cost else 0.0,
            ),
        )

        # Benders bus-balance penalties: violation cost / base × safety factor
        safety = 5.0
        benders = BendersConfig(
            ac_opf_bus_active_power_balance_slack_penalty_per_mw=(
                float(p_bus_vio_cost) / base_mva * safety
            ),
            ac_opf_bus_reactive_power_balance_slack_penalty_per_mvar=(
                float(q_bus_vio_cost) / base_mva * safety
            ),
        )

        return cls(
            penalties=penalties,
            network_rules=network_rules,
            benders=benders,
        )

    @classmethod
    def goc3_default(cls, *args: Any, **kwargs: Any) -> MarketConfig:
        """Alias for :meth:`default` — the GO C3 recipe is the canonical default."""
        return cls.default(*args, **kwargs)

    @classmethod
    def standard(cls, *args: Any, **kwargs: Any) -> MarketConfig:
        """Alias for :meth:`default`, kept for backwards compatibility."""
        return cls.default(*args, **kwargs)

    @classmethod
    def from_preset(cls, preset: str = "default", /, **kwargs: Any) -> MarketConfig:
        """Build a market configuration from a named preset."""
        normalized = preset.strip().lower()
        if normalized in {"default", "goc3", "go_c3", "go-c3", "standard"}:
            return cls.default(**kwargs)
        raise ValueError(f"unknown market preset: {preset!r}")

    def to_penalty_dict(self) -> dict[str, Any]:
        """Build the ``penalty_config`` dict for a dispatch request."""
        return self.penalties.to_dict()

    def apply_defaults_to_request(self, request: dict[str, Any]) -> dict[str, Any]:
        """Fill missing market/network defaults onto a dispatch request."""

        def merge_missing(target: dict[str, Any], defaults: dict[str, Any]) -> dict[str, Any]:
            merged = copy.deepcopy(target)
            for key, value in defaults.items():
                if key not in merged:
                    merged[key] = copy.deepcopy(value)
                elif isinstance(merged[key], dict) and isinstance(value, dict):
                    merged[key] = merge_missing(merged[key], value)
            return merged

        resolved = copy.deepcopy(request)
        market = resolved.setdefault("market", {})
        network = resolved.setdefault("network", {})

        market = merge_missing(market, {"penalty_config": self.to_penalty_dict()})
        network = merge_missing(network, self.network_rules.to_dict())

        resolved["market"] = market
        resolved["network"] = network
        return resolved
