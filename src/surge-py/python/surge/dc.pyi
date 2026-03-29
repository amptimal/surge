# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Type stubs for the canonical ``surge.dc`` surface."""

from dataclasses import dataclass
from typing import Mapping, Optional, Sequence

from ._pf_types import DcPfOptions as DcPfOptions
from ._surge import (
    DcPfResult as DcPfResult,
    LodfMatrixResult as LodfMatrixResult,
    LodfResult as LodfResult,
    N2LodfBatchResult as N2LodfBatchResult,
    N2LodfResult as N2LodfResult,
    OtdfResult as OtdfResult,
    PtdfResult as PtdfResult,
)


@dataclass(frozen=True, slots=True)
class BranchKey:
    from_bus: int
    to_bus: int
    circuit: str | int | None = "1"
    def normalized_circuit(self) -> str: ...


@dataclass(frozen=True, slots=True)
class SlackPolicy:
    mode: str
    weights_by_bus: dict[int, float] | None = None
    headroom_buses: tuple[int, ...] | None = None

    @classmethod
    def single(cls) -> SlackPolicy: ...
    @classmethod
    def weights(cls, weights_by_bus: Mapping[int, float]) -> SlackPolicy: ...
    @classmethod
    def headroom(cls, buses: Sequence[int] | None = None) -> SlackPolicy: ...


@dataclass(slots=True, kw_only=True)
class PtdfRequest:
    monitored_branches: tuple[BranchKey, ...] | None = None
    bus_numbers: tuple[int, ...] | None = None
    slack: SlackPolicy = ...


@dataclass(slots=True, kw_only=True)
class OtdfRequest:
    monitored_branches: tuple[BranchKey, ...]
    outage_branches: tuple[BranchKey, ...]
    bus_numbers: tuple[int, ...] | None = None
    slack: SlackPolicy = ...


@dataclass(slots=True, kw_only=True)
class LodfRequest:
    monitored_branches: tuple[BranchKey, ...] | None = None
    outage_branches: tuple[BranchKey, ...] | None = None


@dataclass(slots=True, kw_only=True)
class LodfMatrixRequest:
    branches: tuple[BranchKey, ...] | None = None


@dataclass(slots=True, kw_only=True)
class N2LodfRequest:
    outage_pair: tuple[BranchKey, BranchKey]
    monitored_branches: tuple[BranchKey, ...] | None = None


@dataclass(slots=True, kw_only=True)
class N2LodfBatchRequest:
    outage_pairs: tuple[tuple[BranchKey, BranchKey], ...]
    monitored_branches: tuple[BranchKey, ...] | None = None


@dataclass(slots=True, kw_only=True)
class DcAnalysisRequest:
    monitored_branches: tuple[BranchKey, ...] | None = None
    ptdf_bus_numbers: tuple[int, ...] | None = None
    otdf_outage_branches: tuple[BranchKey, ...] | None = None
    otdf_bus_numbers: tuple[int, ...] | None = None
    lodf_outage_branches: tuple[BranchKey, ...] | None = None
    n2_outage_pairs: tuple[tuple[BranchKey, BranchKey], ...] | None = None
    pf_options: DcPfOptions = ...
    sensitivity_slack: Optional[SlackPolicy] = None


@dataclass(slots=True)
class DcAnalysisResult:
    power_flow: DcPfResult
    ptdf: PtdfResult
    ptdf_bus_numbers: list[int]
    otdf: OtdfResult | None = None
    otdf_bus_numbers: list[int] = ...
    lodf: LodfResult | None = None
    n2_lodf: N2LodfBatchResult | None = None


class PreparedDcStudy:
    def __init__(self, network) -> None: ...
    def solve_pf(self, options: DcPfOptions | None = None) -> DcPfResult: ...
    def compute_ptdf(self, request: PtdfRequest | None = None) -> PtdfResult: ...
    def compute_lodf(self, request: LodfRequest | None = None) -> LodfResult: ...
    def compute_lodf_matrix(self, request: LodfMatrixRequest | None = None) -> LodfMatrixResult: ...
    def compute_otdf(self, request: OtdfRequest) -> OtdfResult: ...
    def compute_n2_lodf(self, request: N2LodfRequest) -> N2LodfResult: ...
    def compute_n2_lodf_batch(self, request: N2LodfBatchRequest) -> N2LodfBatchResult: ...
    def run_analysis(self, request: DcAnalysisRequest) -> DcAnalysisResult: ...


def prepare_study(network) -> PreparedDcStudy: ...
def compute_ptdf(network, request: PtdfRequest | None = None) -> PtdfResult: ...
def compute_lodf(network, request: LodfRequest | None = None) -> LodfResult: ...
def compute_lodf_matrix(network, request: LodfMatrixRequest | None = None) -> LodfMatrixResult: ...
def compute_otdf(network, request: OtdfRequest) -> OtdfResult: ...
def compute_n2_lodf(network, request: N2LodfRequest) -> N2LodfResult: ...
def compute_n2_lodf_batch(network, request: N2LodfBatchRequest) -> N2LodfBatchResult: ...
def run_analysis(network, request: DcAnalysisRequest) -> DcAnalysisResult: ...


__all__: list[str]
