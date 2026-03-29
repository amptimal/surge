# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from . import _native


def ohm_to_pu(ohm: float, base_kv: float, base_mva: float = 100.0) -> float:
    """Convert an impedance from ohms to per-unit.

    Args:
        ohm: Impedance value in ohms.
        base_kv: Base voltage in kV.
        base_mva: System MVA base (default 100).

    Returns:
        Impedance in per-unit on the given base.
    """
    return _native._units_ohm_to_pu(ohm, base_kv, base_mva)

