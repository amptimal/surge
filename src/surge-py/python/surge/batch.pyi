# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pandas as pd

from ._surge import AcPfResult, DcPfResult, Network, OpfResult, SweepResult, SweepResults


@dataclass
class ScenarioResult:
    case_name: str
    network: Network | None = None
    solution: AcPfResult | DcPfResult | OpfResult | None = None
    error: str | None = None
    wall_time_s: float = 0.0


@dataclass
class BatchResults:
    results: list[ScenarioResult] = ...
    def to_dataframe(self) -> pd.DataFrame: ...
    def compare(self, metric: str = "vm") -> pd.DataFrame: ...
    def violations(
        self,
        vmin: float = 0.95,
        vmax: float = 1.05,
        thermal_pct: float = 100.0,
    ) -> pd.DataFrame: ...
    def __len__(self) -> int: ...
    def __getitem__(self, idx: int) -> ScenarioResult: ...


def batch_solve(
    cases: list[str | Path | Any],
    solver: str = "acpf",
    parallel: bool = True,
    max_workers: int | None = None,
    **solver_kwargs: Any,
) -> BatchResults: ...


parameter_sweep: Any
SweepResult = SweepResult
SweepResults = SweepResults

__all__: list[str]
