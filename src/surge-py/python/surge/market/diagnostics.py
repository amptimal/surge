# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Parse AC-SCED initial-point diagnostics from solve error strings.

The Ipopt backend emits a well-structured diagnostic string when AC-OPF
fails. This module parses it into structured Python data for
programmatic analysis.

Usage::

    from surge.market.diagnostics import parse_initial_point_diagnostic

    result = solve_market_workflow_py(workflow, stop_after_stage=None, ...)
    # or from an exception:
    try:
        solve_market_workflow_py(workflow, ...)
    except SurgeError as e:
        diag = parse_initial_point_diagnostic(str(e))
        for c in diag["top_constraints"]:
            print(f'{c["type"]}({c["bus"]}) vio={c["violation"]:.3f}')
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field


@dataclass
class ConstraintViolation:
    constraint_type: str  # "p_balance", "q_balance", "thermal_from", etc.
    bus_or_id: str
    value: float
    violation: float
    lower_bound: float
    upper_bound: float


@dataclass
class InitialPointDiagnostic:
    max_var_violation: float = 0.0
    max_constraint_violation: float = 0.0
    top_constraints: list[ConstraintViolation] = field(default_factory=list)
    failing_period: int | None = None
    attempt_label: str | None = None


_FLOAT = r"[-+]?[0-9.]+(?:[eE][-+]?[0-9]+)?"

_IP_PATTERN = re.compile(
    r"initial_point_v2="
    rf"max_var_violation=({_FLOAT});\s*"
    rf"max_constraint_violation=({_FLOAT});\s*"
    r"top_constraints=\[(.+)\]"
)

_CONSTRAINT_PATTERN = re.compile(
    r"(\w+)\((?:bus|branch)=(\w+)\):\s*"
    rf"g=({_FLOAT})\s+"
    rf"bounds=\[({_FLOAT}),\s*({_FLOAT})\]\s+"
    rf"vio=({_FLOAT})"
)

_PERIOD_PATTERN = re.compile(r"AC-SCED period (\d+)")
_ATTEMPT_PATTERN = re.compile(r"\[([^\]]+)\] solver error:")


def parse_initial_point_diagnostic(error_text: str) -> InitialPointDiagnostic | None:
    """Extract structured initial-point data from an AC-SCED error string.

    Returns ``None`` if no diagnostic is found in the text.
    """
    m = _IP_PATTERN.search(error_text)
    if not m:
        return None

    constraints = []
    for cm in _CONSTRAINT_PATTERN.finditer(m.group(3)):
        constraints.append(ConstraintViolation(
            constraint_type=cm.group(1),
            bus_or_id=cm.group(2),
            value=float(cm.group(3)),
            violation=float(cm.group(6)),
            lower_bound=float(cm.group(4)),
            upper_bound=float(cm.group(5)),
        ))

    period_m = _PERIOD_PATTERN.search(error_text)
    attempt_m = _ATTEMPT_PATTERN.search(error_text)

    return InitialPointDiagnostic(
        max_var_violation=float(m.group(1)),
        max_constraint_violation=float(m.group(2)),
        top_constraints=constraints,
        failing_period=int(period_m.group(1)) if period_m else None,
        attempt_label=attempt_m.group(1) if attempt_m else None,
    )


def parse_all_attempt_diagnostics(error_text: str) -> list[InitialPointDiagnostic]:
    """Parse diagnostics from ALL retry-grid attempts in an error string.

    Each ``[label] solver error: AC-SCED period N: ...`` block produces
    one ``InitialPointDiagnostic``.
    """
    results = []
    for attempt_block in re.split(r"(?=\[[\w/=]+\] solver error:)", error_text):
        diag = parse_initial_point_diagnostic(attempt_block)
        if diag is not None:
            results.append(diag)
    return results
