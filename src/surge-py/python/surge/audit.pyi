# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from dataclasses import dataclass

import pandas as pd

from ._surge import Network


@dataclass(frozen=True)
class ModelIssue:
    severity: str
    category: str
    message: str
    element_type: str
    element_id: str


def audit_model(network: Network) -> list[ModelIssue]: ...
def audit_dataframe(network: Network) -> pd.DataFrame: ...
