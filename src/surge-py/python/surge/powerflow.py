# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Power flow solvers (DC, AC Newton-Raphson, FDPF)."""
from __future__ import annotations

from typing import TypeVar

from . import _native
from ._pf_types import AcPfOptions, DcPfOptions, FdpfOptions

PreparedAcPf = _native.PreparedAcPf
YBusResult = _native.YBusResult
JacobianResult = _native.JacobianResult
_OptionsT = TypeVar("_OptionsT")


def _require_options(name: str, options: _OptionsT | None, option_type: type[_OptionsT]) -> _OptionsT:
    if options is None:
        resolved = option_type()  # type: ignore[call-arg]
    elif isinstance(options, option_type):
        resolved = options
    else:
        raise TypeError(f"{name}() expected {option_type.__name__} for options")
    return resolved


def solve_dc_pf(network, options: DcPfOptions | None = None, /):
    """Solve a DC power flow (linearized, lossless approximation).

    Args:
        network: Power system network.
        options: DC power flow options (defaults to standard settings).

    Returns:
        DcPfResult with bus voltage angles and branch MW flows.
    """
    resolved = _require_options("solve_dc_pf", options, DcPfOptions)
    return _native.solve_dc_pf(network, **resolved.to_native_kwargs(network))


def solve_ac_pf(network, options: AcPfOptions | None = None, /):
    """Solve a full AC power flow using Newton-Raphson iteration.

    Args:
        network: Power system network.
        options: AC power flow options (tolerance, max iterations, etc.).

    Returns:
        AcPfResult with bus voltages, branch P/Q flows, and convergence info.
    """
    resolved = _require_options("solve_ac_pf", options, AcPfOptions)
    return _native.solve_ac_pf(network, **resolved.to_native_kwargs(network))


def solve_fdpf(network, options: FdpfOptions | None = None, /):
    """Solve an AC power flow using the Fast Decoupled method (XB or BX variant).

    Args:
        network: Power system network.
        options: FDPF options (variant, tolerance, max iterations, etc.).

    Returns:
        AcPfResult with bus voltages, branch P/Q flows, and convergence info.
    """
    resolved = _require_options("solve_fdpf", options, FdpfOptions)
    return _native.solve_fdpf(network, **resolved.to_native_kwargs(network))


__all__ = [
    "AcPfOptions",
    "DcPfOptions",
    "FdpfOptions",
    "JacobianResult",
    "PreparedAcPf",
    "YBusResult",
    "solve_ac_pf",
    "solve_dc_pf",
    "solve_fdpf",
]
