# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from typing import Any, Optional

from ._pf_types import AcPfOptions, DcPfOptions, FdpfOptions
from ._surge import AcPfResult, DcPfResult, JacobianResult, PreparedAcPf, YBusResult


def solve_dc_pf(network: Any, options: Optional[DcPfOptions] = None, /) -> DcPfResult: ...


def solve_ac_pf(network: Any, options: Optional[AcPfOptions] = None, /) -> AcPfResult: ...


def solve_fdpf(network: Any, options: Optional[FdpfOptions] = None, /) -> AcPfResult: ...
