# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

import pandas as pd

from ._surge import Network


def from_dataframes(
    buses: pd.DataFrame,
    branches: pd.DataFrame,
    generators: pd.DataFrame,
    *,
    base_mva: float = 100.0,
    name: str = "",
) -> Network: ...
