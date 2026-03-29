# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Canonical DC power-flow and sensitivity API."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Mapping, Sequence

from . import _native
from ._pf_types import DcPfOptions

DcPfResult = _native.DcPfResult
PtdfResult = _native.PtdfResult
LodfResult = _native.LodfResult
LodfMatrixResult = _native.LodfMatrixResult
N2LodfResult = _native.N2LodfResult
N2LodfBatchResult = _native.N2LodfBatchResult
OtdfResult = _native.OtdfResult


@dataclass(frozen=True, slots=True)
class BranchKey:
    """Identifies a branch by its from-bus, to-bus, and circuit ID.

    Args:
        from_bus: Source bus number.
        to_bus: Destination bus number.
        circuit: Circuit identifier (defaults to "1").
    """

    from_bus: int
    to_bus: int
    circuit: str | int | None = "1"

    def normalized_circuit(self) -> str:
        if self.circuit is None:
            return "1"
        return str(self.circuit)


@dataclass(frozen=True, slots=True)
class SlackPolicy:
    """Defines how slack (mismatch) is distributed across buses in DC analysis.

    Use the factory classmethods ``single()``, ``weights()``, or ``headroom()``
    instead of constructing directly.
    """

    mode: str
    weights_by_bus: dict[int, float] | None = None
    headroom_buses: tuple[int, ...] | None = None

    def __post_init__(self) -> None:
        if self.mode not in {"single", "weights", "headroom"}:
            raise ValueError("SlackPolicy.mode must be 'single', 'weights', or 'headroom'")
        if self.mode == "single":
            if self.weights_by_bus is not None or self.headroom_buses is not None:
                raise ValueError("single-slack policy does not accept weights or buses")
            return
        if self.mode == "weights":
            if not self.weights_by_bus:
                raise ValueError("weighted slack policy requires at least one bus weight")
            if self.headroom_buses is not None:
                raise ValueError("weighted slack policy does not accept headroom buses")
            normalized_weights = {
                int(bus): float(weight) for bus, weight in self.weights_by_bus.items()
            }
            if any(weight <= 0.0 for weight in normalized_weights.values()):
                raise ValueError("weighted slack policy requires strictly positive bus weights")
            object.__setattr__(
                self,
                "weights_by_bus",
                normalized_weights,
            )
            return
        if self.weights_by_bus is not None:
            raise ValueError("headroom slack policy does not accept explicit weights")
        if self.headroom_buses is not None:
            normalized_buses = tuple(int(bus) for bus in self.headroom_buses)
            if not normalized_buses:
                raise ValueError("headroom slack policy buses must not be empty")
            object.__setattr__(self, "headroom_buses", normalized_buses)

    @classmethod
    def single(cls) -> "SlackPolicy":
        """Create a single-slack policy that assigns all mismatch to the reference bus."""
        return cls("single")

    @classmethod
    def weights(cls, weights_by_bus: Mapping[int, float]) -> "SlackPolicy":
        """Create a weighted distributed-slack policy.

        Args:
            weights_by_bus: Mapping of bus number to positive participation weight.
        """
        return cls("weights", dict(weights_by_bus), None)

    @classmethod
    def headroom(cls, buses: Sequence[int] | None = None) -> "SlackPolicy":
        """Create a headroom-based distributed-slack policy.

        Args:
            buses: Optional subset of bus numbers eligible for slack. When None,
                all generator buses participate proportionally to their headroom.
        """
        return cls("headroom", None, None if buses is None else tuple(buses))


def _coerce_branch_keys(
    values: Sequence[BranchKey] | None,
    *,
    field_name: str,
) -> tuple[BranchKey, ...] | None:
    if values is None:
        return None
    keys = tuple(values)
    for value in keys:
        if not isinstance(value, BranchKey):
            raise TypeError(f"{field_name} must contain only BranchKey values")
    return keys


def _coerce_outage_pairs(
    values: Sequence[tuple[BranchKey, BranchKey]] | None,
    *,
    field_name: str,
) -> tuple[tuple[BranchKey, BranchKey], ...] | None:
    if values is None:
        return None
    pairs = tuple(values)
    for pair in pairs:
        if len(pair) != 2 or not all(isinstance(branch, BranchKey) for branch in pair):
            raise TypeError(f"{field_name} must contain only (BranchKey, BranchKey) pairs")
    return pairs


def _coerce_bus_numbers(values: Sequence[int] | None, *, field_name: str) -> tuple[int, ...] | None:
    if values is None:
        return None
    try:
        return tuple(int(value) for value in values)
    except Exception as exc:  # pragma: no cover - defensive
        raise TypeError(f"{field_name} must be a sequence of bus numbers") from exc


def _require_slack_policy(value: object, *, field_name: str) -> SlackPolicy:
    if not isinstance(value, SlackPolicy):
        raise TypeError(f"{field_name} must be a SlackPolicy")
    return value


@dataclass(slots=True, kw_only=True)
class PtdfRequest:
    """Request parameters for Power Transfer Distribution Factor computation.

    Args:
        monitored_branches: Branches to include in the PTDF rows (None = all).
        bus_numbers: Bus subset for the PTDF columns (None = all).
        slack: Slack distribution policy for the reference bus treatment.
    """

    monitored_branches: tuple[BranchKey, ...] | None = None
    bus_numbers: tuple[int, ...] | None = None
    slack: SlackPolicy = field(default_factory=SlackPolicy.single)

    def __post_init__(self) -> None:
        self.slack = _require_slack_policy(self.slack, field_name="PtdfRequest.slack")
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="PtdfRequest.monitored_branches",
        )
        self.bus_numbers = _coerce_bus_numbers(
            self.bus_numbers,
            field_name="PtdfRequest.bus_numbers",
        )


@dataclass(slots=True, kw_only=True)
class OtdfRequest:
    """Request parameters for Outage Transfer Distribution Factor computation.

    Args:
        monitored_branches: Branches to monitor for flow redistribution.
        outage_branches: Branches to simulate as outaged.
        bus_numbers: Optional bus subset for the OTDF columns.
        slack: Slack distribution policy.
    """

    monitored_branches: tuple[BranchKey, ...]
    outage_branches: tuple[BranchKey, ...]
    bus_numbers: tuple[int, ...] | None = None
    slack: SlackPolicy = field(default_factory=SlackPolicy.single)

    def __post_init__(self) -> None:
        self.slack = _require_slack_policy(self.slack, field_name="OtdfRequest.slack")
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="OtdfRequest.monitored_branches",
        ) or ()
        self.outage_branches = _coerce_branch_keys(
            self.outage_branches,
            field_name="OtdfRequest.outage_branches",
        ) or ()
        self.bus_numbers = _coerce_bus_numbers(
            self.bus_numbers,
            field_name="OtdfRequest.bus_numbers",
        )
        if not self.monitored_branches:
            raise ValueError("OtdfRequest.monitored_branches must not be empty")
        if not self.outage_branches:
            raise ValueError("OtdfRequest.outage_branches must not be empty")


@dataclass(slots=True, kw_only=True)
class LodfRequest:
    """Request parameters for Line Outage Distribution Factor computation.

    Args:
        monitored_branches: Branches to monitor (None = all).
        outage_branches: Branches to simulate as outaged (None = all).
    """

    monitored_branches: tuple[BranchKey, ...] | None = None
    outage_branches: tuple[BranchKey, ...] | None = None

    def __post_init__(self) -> None:
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="LodfRequest.monitored_branches",
        )
        self.outage_branches = _coerce_branch_keys(
            self.outage_branches,
            field_name="LodfRequest.outage_branches",
        )


@dataclass(slots=True, kw_only=True)
class LodfMatrixRequest:
    """Request parameters for a full LODF matrix computation.

    Args:
        branches: Subset of branches for the square LODF matrix (None = all).
    """

    branches: tuple[BranchKey, ...] | None = None

    def __post_init__(self) -> None:
        self.branches = _coerce_branch_keys(
            self.branches,
            field_name="LodfMatrixRequest.branches",
        )


@dataclass(slots=True, kw_only=True)
class N2LodfRequest:
    """Request parameters for N-2 LODF computation (simultaneous double outage).

    Args:
        outage_pair: Pair of two distinct branches to outage simultaneously.
        monitored_branches: Branches to monitor (None = all).
    """

    outage_pair: tuple[BranchKey, BranchKey]
    monitored_branches: tuple[BranchKey, ...] | None = None

    def __post_init__(self) -> None:
        self.outage_pair = _coerce_outage_pairs(
            (self.outage_pair,),
            field_name="N2LodfRequest.outage_pair",
        )[0]
        if self.outage_pair[0] == self.outage_pair[1]:
            raise ValueError("N2LodfRequest.outage_pair must contain two distinct branches")
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="N2LodfRequest.monitored_branches",
        )


@dataclass(slots=True, kw_only=True)
class N2LodfBatchRequest:
    """Request parameters for batch N-2 LODF computation over multiple outage pairs.

    Args:
        outage_pairs: Sequence of (BranchKey, BranchKey) pairs to outage simultaneously.
        monitored_branches: Branches to monitor (None = all).
    """

    outage_pairs: tuple[tuple[BranchKey, BranchKey], ...]
    monitored_branches: tuple[BranchKey, ...] | None = None

    def __post_init__(self) -> None:
        self.outage_pairs = _coerce_outage_pairs(
            self.outage_pairs,
            field_name="N2LodfBatchRequest.outage_pairs",
        ) or ()
        if not self.outage_pairs:
            raise ValueError("N2LodfBatchRequest.outage_pairs must not be empty")
        if any(first == second for first, second in self.outage_pairs):
            raise ValueError("N2LodfBatchRequest.outage_pairs must contain only distinct branch pairs")
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="N2LodfBatchRequest.monitored_branches",
        )


@dataclass(slots=True, kw_only=True)
class DcAnalysisRequest:
    """Combined request for DC power flow, PTDF, OTDF, LODF, and N-2 LODF analysis.

    Bundles all sensitivity requests into a single pass over the network.
    """

    monitored_branches: tuple[BranchKey, ...] | None = None
    ptdf_bus_numbers: tuple[int, ...] | None = None
    otdf_outage_branches: tuple[BranchKey, ...] | None = None
    otdf_bus_numbers: tuple[int, ...] | None = None
    lodf_outage_branches: tuple[BranchKey, ...] | None = None
    n2_outage_pairs: tuple[tuple[BranchKey, BranchKey], ...] | None = None
    pf_options: DcPfOptions = field(default_factory=DcPfOptions)
    sensitivity_slack: SlackPolicy | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.pf_options, DcPfOptions):
            raise TypeError("DcAnalysisRequest.pf_options must be a DcPfOptions")
        if self.sensitivity_slack is not None:
            self.sensitivity_slack = _require_slack_policy(
                self.sensitivity_slack,
                field_name="DcAnalysisRequest.sensitivity_slack",
            )
        self.monitored_branches = _coerce_branch_keys(
            self.monitored_branches,
            field_name="DcAnalysisRequest.monitored_branches",
        )
        self.ptdf_bus_numbers = _coerce_bus_numbers(
            self.ptdf_bus_numbers,
            field_name="DcAnalysisRequest.ptdf_bus_numbers",
        )
        self.otdf_outage_branches = _coerce_branch_keys(
            self.otdf_outage_branches,
            field_name="DcAnalysisRequest.otdf_outage_branches",
        )
        self.otdf_bus_numbers = _coerce_bus_numbers(
            self.otdf_bus_numbers,
            field_name="DcAnalysisRequest.otdf_bus_numbers",
        )
        self.lodf_outage_branches = _coerce_branch_keys(
            self.lodf_outage_branches,
            field_name="DcAnalysisRequest.lodf_outage_branches",
        )
        self.n2_outage_pairs = _coerce_outage_pairs(
            self.n2_outage_pairs,
            field_name="DcAnalysisRequest.n2_outage_pairs",
        )


@dataclass(slots=True)
class DcAnalysisResult:
    """Results from a combined DC analysis run.

    Contains the DC power flow solution, PTDF matrix, and optional OTDF, LODF,
    and N-2 LODF results depending on what was requested.
    """

    power_flow: DcPfResult
    ptdf: PtdfResult
    ptdf_bus_numbers: list[int]
    otdf: OtdfResult | None = None
    otdf_bus_numbers: list[int] = field(default_factory=list)
    lodf: LodfResult | None = None
    n2_lodf: N2LodfBatchResult | None = None


def _require_dc_pf_options(options: DcPfOptions | None) -> DcPfOptions:
    if options is None:
        return DcPfOptions()
    if not isinstance(options, DcPfOptions):
        raise TypeError("expected DcPfOptions")
    return options


def _branch_index(network, branch: BranchKey) -> int:
    return int(
        network.branch_index(
            int(branch.from_bus),
            int(branch.to_bus),
            branch.normalized_circuit(),
        )
    )


def _branch_indices(network, branches: tuple[BranchKey, ...] | None) -> list[int] | None:
    if branches is None:
        return None
    return [_branch_index(network, branch) for branch in branches]


def _all_branch_keys(network) -> tuple[BranchKey, ...]:
    return tuple(
        BranchKey(from_bus, to_bus, circuit)
        for from_bus, to_bus, circuit in zip(
            network.branch_from,
            network.branch_to,
            network.branch_circuit,
        )
    )


def _outage_pairs(network, pairs: tuple[tuple[BranchKey, BranchKey], ...] | None) -> list[tuple[int, int]] | None:
    if pairs is None:
        return None
    return [(_branch_index(network, first), _branch_index(network, second)) for first, second in pairs]


def _bus_indices(network, bus_numbers: tuple[int, ...] | None) -> list[int] | None:
    if bus_numbers is None:
        return None
    return [int(network.bus_index(bus_number)) for bus_number in bus_numbers]


def _native_slack_kwargs(slack: SlackPolicy) -> dict[str, object]:
    if slack.mode == "single":
        return {}
    if slack.mode == "weights":
        return {"slack_weights": dict(slack.weights_by_bus or {})}
    if slack.headroom_buses is None:
        return {"headroom_slack": True}
    return {"headroom_slack_buses": list(slack.headroom_buses)}


def _derived_sensitivity_slack(pf_options: DcPfOptions, sensitivity_slack: SlackPolicy | None) -> SlackPolicy:
    if sensitivity_slack is not None:
        return sensitivity_slack
    if pf_options.participation_factors:
        return SlackPolicy.weights(pf_options.participation_factors)
    if pf_options.headroom_slack_buses:
        return SlackPolicy.headroom(pf_options.headroom_slack_buses)
    if pf_options.headroom_slack:
        return SlackPolicy.headroom()
    return SlackPolicy.single()


class PreparedDcStudy:
    """Pre-factored DC study that can compute power flow and sensitivities efficiently.

    Build via ``prepare_study(network)`` or construct directly. The network
    factorization is reused across all subsequent solve and compute calls.
    """

    __slots__ = ("_network", "_native")

    def __init__(self, network) -> None:
        self._network = network
        self._native = _native.prepare_dc_study(network)

    def solve_pf(self, options: DcPfOptions | None = None) -> DcPfResult:
        """Solve DC power flow using the pre-factored study.

        Args:
            options: DC power flow options (defaults to standard settings).

        Returns:
            DcPfResult with bus angles and branch flows.
        """
        resolved = _require_dc_pf_options(options)
        return self._native.solve_pf(**resolved.to_native_kwargs(self._network))

    def compute_ptdf(self, request: PtdfRequest | None = None) -> PtdfResult:
        """Compute Power Transfer Distribution Factors.

        Args:
            request: PTDF request specifying monitored branches and bus subset.

        Returns:
            PtdfResult with the PTDF matrix and metadata.
        """
        resolved = request or PtdfRequest()
        if not isinstance(resolved, PtdfRequest):
            raise TypeError("compute_ptdf() requires a PtdfRequest")
        return self._native.compute_ptdf(
            monitored_branches=_branch_indices(self._network, resolved.monitored_branches),
            bus_indices=_bus_indices(self._network, resolved.bus_numbers),
            **_native_slack_kwargs(resolved.slack),
        )

    def compute_lodf(self, request: LodfRequest | None = None) -> LodfResult:
        """Compute Line Outage Distribution Factors.

        Args:
            request: LODF request specifying monitored and outage branches.

        Returns:
            LodfResult with the LODF matrix and metadata.
        """
        resolved = request or LodfRequest()
        if not isinstance(resolved, LodfRequest):
            raise TypeError("compute_lodf() requires a LodfRequest")
        return self._native.compute_lodf(
            monitored_branches=_branch_indices(self._network, resolved.monitored_branches),
            outage_branches=_branch_indices(self._network, resolved.outage_branches),
        )

    def compute_lodf_matrix(self, request: LodfMatrixRequest | None = None) -> LodfMatrixResult:
        """Compute the full square LODF matrix.

        Args:
            request: Request specifying the branch subset (None = all branches).

        Returns:
            LodfMatrixResult with the square LODF matrix.
        """
        resolved = request or LodfMatrixRequest()
        if not isinstance(resolved, LodfMatrixRequest):
            raise TypeError("compute_lodf_matrix() requires a LodfMatrixRequest")
        return self._native.compute_lodf_matrix(
            branches=_branch_indices(self._network, resolved.branches),
        )

    def compute_otdf(self, request: OtdfRequest) -> OtdfResult:
        """Compute Outage Transfer Distribution Factors.

        Args:
            request: OTDF request with monitored branches, outage branches, and bus subset.

        Returns:
            OtdfResult with the OTDF tensor and metadata.
        """
        if not isinstance(request, OtdfRequest):
            raise TypeError("compute_otdf() requires an OtdfRequest")
        return self._native.compute_otdf(
            monitored_branches=_branch_indices(self._network, request.monitored_branches),
            outage_branches=_branch_indices(self._network, request.outage_branches),
            bus_indices=_bus_indices(self._network, request.bus_numbers),
            **_native_slack_kwargs(request.slack),
        )

    def compute_n2_lodf(self, request: N2LodfRequest) -> N2LodfResult:
        """Compute N-2 LODF for a single simultaneous double-branch outage.

        Args:
            request: N-2 LODF request with the outage pair and monitored branches.

        Returns:
            N2LodfResult with flow redistribution factors.
        """
        if not isinstance(request, N2LodfRequest):
            raise TypeError("compute_n2_lodf() requires an N2LodfRequest")
        first, second = request.outage_pair
        return self._native.compute_n2_lodf(
            outage_pair=(_branch_index(self._network, first), _branch_index(self._network, second)),
            monitored_branches=_branch_indices(self._network, request.monitored_branches),
        )

    def compute_n2_lodf_batch(self, request: N2LodfBatchRequest) -> N2LodfBatchResult:
        """Compute N-2 LODF for multiple simultaneous double-branch outage pairs.

        Args:
            request: Batch request with outage pairs and monitored branches.

        Returns:
            N2LodfBatchResult with flow redistribution factors for each pair.
        """
        if not isinstance(request, N2LodfBatchRequest):
            raise TypeError("compute_n2_lodf_batch() requires an N2LodfBatchRequest")
        return self._native.compute_n2_lodf_batch(
            outage_pairs=_outage_pairs(self._network, request.outage_pairs),
            monitored_branches=_branch_indices(self._network, request.monitored_branches),
        )

    def run_analysis(self, request: DcAnalysisRequest) -> DcAnalysisResult:
        """Run a combined DC analysis (power flow + all requested sensitivities).

        Args:
            request: DcAnalysisRequest bundling power flow options and sensitivity specs.

        Returns:
            DcAnalysisResult with power flow, PTDF, and optional OTDF/LODF/N-2 results.
        """
        if not isinstance(request, DcAnalysisRequest):
            raise TypeError("run_analysis() requires a DcAnalysisRequest")
        pf_options = _require_dc_pf_options(request.pf_options)
        sensitivity_slack = _derived_sensitivity_slack(pf_options, request.sensitivity_slack)
        power_flow = self.solve_pf(pf_options)
        ptdf = self.compute_ptdf(
            PtdfRequest(
                monitored_branches=request.monitored_branches,
                bus_numbers=request.ptdf_bus_numbers,
                slack=sensitivity_slack,
            )
        )
        otdf = None
        if request.otdf_outage_branches:
            otdf = self.compute_otdf(
                OtdfRequest(
                    monitored_branches=request.monitored_branches or _all_branch_keys(self._network),
                    outage_branches=request.otdf_outage_branches,
                    bus_numbers=request.otdf_bus_numbers,
                    slack=sensitivity_slack,
                )
            )
        lodf = None
        if request.lodf_outage_branches:
            lodf = self.compute_lodf(
                LodfRequest(
                    monitored_branches=request.monitored_branches,
                    outage_branches=request.lodf_outage_branches,
                )
            )
        n2_lodf = None
        if request.n2_outage_pairs:
            n2_lodf = self.compute_n2_lodf_batch(
                N2LodfBatchRequest(
                    outage_pairs=request.n2_outage_pairs,
                    monitored_branches=request.monitored_branches,
                )
            )
        return DcAnalysisResult(
            power_flow=power_flow,
            ptdf=ptdf,
            ptdf_bus_numbers=list(request.ptdf_bus_numbers or ptdf.bus_numbers),
            otdf=otdf,
            otdf_bus_numbers=list(request.otdf_bus_numbers or (otdf.bus_numbers if otdf else [])),
            lodf=lodf,
            n2_lodf=n2_lodf,
        )

    def __repr__(self) -> str:
        return repr(self._native)


def prepare_study(network) -> PreparedDcStudy:
    """Pre-factor a network for efficient repeated DC power flow and sensitivity queries.

    Args:
        network: Power system network to factorize.

    Returns:
        PreparedDcStudy that can be reused for multiple solve/compute calls.
    """
    return PreparedDcStudy(network)


def compute_ptdf(network, request: PtdfRequest | None = None) -> PtdfResult:
    """Compute Power Transfer Distribution Factors for a network.

    Convenience wrapper that prepares a study and computes PTDFs in one call.

    Args:
        network: Power system network.
        request: Optional PtdfRequest to filter branches and buses.

    Returns:
        PtdfResult with the PTDF matrix and metadata.
    """
    return prepare_study(network).compute_ptdf(request)


def compute_lodf(network, request: LodfRequest | None = None) -> LodfResult:
    """Compute Line Outage Distribution Factors for a network.

    Args:
        network: Power system network.
        request: Optional LodfRequest to filter monitored and outage branches.

    Returns:
        LodfResult with the LODF matrix and metadata.
    """
    return prepare_study(network).compute_lodf(request)


def compute_lodf_matrix(network, request: LodfMatrixRequest | None = None) -> LodfMatrixResult:
    """Compute the full square LODF matrix for a network.

    Args:
        network: Power system network.
        request: Optional request to restrict the branch subset.

    Returns:
        LodfMatrixResult with the square LODF matrix.
    """
    return prepare_study(network).compute_lodf_matrix(request)


def compute_otdf(network, request: OtdfRequest) -> OtdfResult:
    """Compute Outage Transfer Distribution Factors for a network.

    Args:
        network: Power system network.
        request: OtdfRequest with monitored branches, outage branches, and bus subset.

    Returns:
        OtdfResult with the OTDF tensor and metadata.
    """
    return prepare_study(network).compute_otdf(request)


def compute_n2_lodf(network, request: N2LodfRequest) -> N2LodfResult:
    """Compute N-2 LODF for a single double-branch outage on a network.

    Args:
        network: Power system network.
        request: N2LodfRequest with the outage pair and monitored branches.

    Returns:
        N2LodfResult with flow redistribution factors.
    """
    return prepare_study(network).compute_n2_lodf(request)


def compute_n2_lodf_batch(network, request: N2LodfBatchRequest) -> N2LodfBatchResult:
    """Compute N-2 LODF for multiple double-branch outage pairs on a network.

    Args:
        network: Power system network.
        request: N2LodfBatchRequest with outage pairs and monitored branches.

    Returns:
        N2LodfBatchResult with flow redistribution factors for each pair.
    """
    return prepare_study(network).compute_n2_lodf_batch(request)


def run_analysis(network, request: DcAnalysisRequest) -> DcAnalysisResult:
    """Run a combined DC analysis (power flow + sensitivities) on a network.

    Args:
        network: Power system network.
        request: DcAnalysisRequest bundling all desired analysis components.

    Returns:
        DcAnalysisResult with power flow, PTDF, and optional OTDF/LODF/N-2 results.
    """
    return prepare_study(network).run_analysis(request)


__all__ = [
    "BranchKey",
    "DcAnalysisRequest",
    "DcAnalysisResult",
    "DcPfOptions",
    "DcPfResult",
    "LodfMatrixRequest",
    "LodfMatrixResult",
    "LodfRequest",
    "LodfResult",
    "N2LodfBatchRequest",
    "N2LodfBatchResult",
    "N2LodfRequest",
    "N2LodfResult",
    "OtdfRequest",
    "OtdfResult",
    "PreparedDcStudy",
    "PtdfRequest",
    "PtdfResult",
    "SlackPolicy",
    "compute_lodf",
    "compute_lodf_matrix",
    "compute_n2_lodf",
    "compute_n2_lodf_batch",
    "compute_otdf",
    "compute_ptdf",
    "prepare_study",
    "run_analysis",
]
