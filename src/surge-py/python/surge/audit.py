# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Model data quality audit — engineering reasonableness checks."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ._surge import Network


@dataclass(frozen=True)
class ModelIssue:
    """A single finding from the model audit.

    Attributes:
        severity: ``"error"``, ``"warning"``, or ``"info"``.
        category: E.g. ``"topology"``, ``"impedance"``, ``"voltage"``,
            ``"generation"``, ``"ratings"``, ``"load"``, ``"transformer"``.
        message: Human-readable description.
        element_type: ``"bus"``, ``"branch"``, ``"generator"``, or ``"load"``.
        element_id: E.g. ``"bus:118"``, ``"branch:1→2(1)"``, ``"gen:5/1"``.
    """

    severity: str
    category: str
    message: str
    element_type: str
    element_id: str


def audit_model(network: Network) -> list[ModelIssue]:
    """Run engineering reasonableness checks on a network.

    This is complementary to ``Network.validate()`` which checks structural
    correctness (will-the-solver-crash).  ``audit_model`` checks whether the
    data *makes physical sense* for a transmission network.

    Returns:
        List of :class:`ModelIssue` findings sorted by severity then element.
    """
    issues: list[ModelIssue] = []
    issues.extend(_check_topology(network))
    issues.extend(_check_impedance(network))
    issues.extend(_check_voltage(network))
    issues.extend(_check_generation(network))
    issues.extend(_check_ratings(network))
    issues.extend(_check_loads(network))
    issues.extend(_check_transformers(network))
    # Sort: errors first, then warnings, then info.
    _sev_order = {"error": 0, "warning": 1, "info": 2}
    issues.sort(key=lambda i: (_sev_order.get(i.severity, 3), i.element_id))
    return issues


def audit_dataframe(network: Network):
    """Run audit and return results as a pandas DataFrame.

    Columns: severity, category, element_type, element_id, message.
    """
    import pandas as pd

    issues = audit_model(network)
    return pd.DataFrame(
        [
            {
                "severity": i.severity,
                "category": i.category,
                "element_type": i.element_type,
                "element_id": i.element_id,
                "message": i.message,
            }
            for i in issues
        ]
    )


# ------------------------------------------------------------------
# Check functions
# ------------------------------------------------------------------


def _check_topology(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    bus_nums = set(net.bus_numbers)
    from_buses = list(net.branch_from)
    to_buses = list(net.branch_to)
    in_service = list(net.branch_in_service)
    gen_buses = set(net.gen_buses)
    bus_pd = dict(zip(net.bus_numbers, net.bus_pd))
    bus_qd = dict(zip(net.bus_numbers, net.bus_qd))

    # Build adjacency for in-service branches.
    connected: set[int] = set()
    for i in range(len(from_buses)):
        if in_service[i]:
            connected.add(from_buses[i])
            connected.add(to_buses[i])

    for bus in bus_nums:
        has_gen = bus in gen_buses
        has_load = abs(bus_pd.get(bus, 0)) > 1e-10 or abs(bus_qd.get(bus, 0)) > 1e-10
        if bus not in connected:
            if has_gen or has_load:
                issues.append(
                    ModelIssue(
                        "warning",
                        "topology",
                        "Isolated bus with no in-service branches connected",
                        "bus",
                        f"bus:{bus}",
                    )
                )
            else:
                issues.append(
                    ModelIssue(
                        "info",
                        "topology",
                        "Orphan bus: no branches, generators, or load",
                        "bus",
                        f"bus:{bus}",
                    )
                )
    return issues


def _check_impedance(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    from_buses = list(net.branch_from)
    to_buses = list(net.branch_to)
    circuits = list(net.branch_circuit)
    r_vals = list(net.branch_r)
    x_vals = list(net.branch_x)
    taps = list(net.branch_tap)

    for i in range(len(from_buses)):
        f, t, c = from_buses[i], to_buses[i], circuits[i]
        eid = f"branch:{f}→{t}({c})"
        r, x = r_vals[i], x_vals[i]

        # Zero impedance (excluding transformers with tap != 1.0 which may be
        # phase shifters or modeled differently).
        is_xfmr = abs(taps[i] - 1.0) > 1e-6
        if abs(r) < 1e-12 and abs(x) < 1e-12 and not is_xfmr:
            issues.append(
                ModelIssue("warning", "impedance", "Zero impedance (R=0, X=0)", "branch", eid)
            )
            continue

        if abs(x) > 1e-12:
            xr = abs(x / r) if abs(r) > 1e-12 else float("inf")
            if xr < 1.0 and not is_xfmr:
                issues.append(
                    ModelIssue(
                        "warning",
                        "impedance",
                        f"X/R ratio = {xr:.2f} < 1.0 (unusually resistive for transmission)",
                        "branch",
                        eid,
                    )
                )

        if x < 0 and not is_xfmr:
            issues.append(
                ModelIssue(
                    "warning",
                    "impedance",
                    f"Negative reactance X = {x:.6f} (non-physical for lines)",
                    "branch",
                    eid,
                )
            )

    return issues


def _check_voltage(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    bus_nums = list(net.bus_numbers)
    bus_vm = list(net.bus_vm)
    bus_vmin = list(net.bus_vmin)
    bus_vmax = list(net.bus_vmax)

    for i in range(len(bus_nums)):
        eid = f"bus:{bus_nums[i]}"
        vm = bus_vm[i]
        if vm < 0.9 or vm > 1.1:
            issues.append(
                ModelIssue(
                    "warning",
                    "voltage",
                    f"Initial Vm = {vm:.4f} outside [0.9, 1.1] pu",
                    "bus",
                    eid,
                )
            )
        if bus_vmin[i] > bus_vmax[i]:
            issues.append(
                ModelIssue(
                    "error",
                    "voltage",
                    f"Vmin ({bus_vmin[i]:.4f}) > Vmax ({bus_vmax[i]:.4f})",
                    "bus",
                    eid,
                )
            )

    # Generator voltage setpoints.
    gen_buses = list(net.gen_buses)
    gen_ids = list(net.gen_machine_id)
    gen_vs = list(net.gen_vs_pu)
    gen_in_service = list(net.gen_in_service)
    for i in range(len(gen_buses)):
        if not gen_in_service[i]:
            continue
        vs = gen_vs[i]
        if vs < 0.9 or vs > 1.1:
            issues.append(
                ModelIssue(
                    "warning",
                    "voltage",
                    f"Generator Vs = {vs:.4f} outside [0.9, 1.1] pu",
                    "generator",
                    f"gen:{gen_buses[i]}/{gen_ids[i]}",
                )
            )

    return issues


def _check_generation(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    gen_buses = list(net.gen_buses)
    gen_ids = list(net.gen_machine_id)
    gen_p = list(net.gen_p)
    gen_pmax = [g.pmax_mw for g in net.generators]
    gen_pmin = [g.pmin_mw for g in net.generators]
    gen_qmax = list(net.gen_qmax)
    gen_qmin = list(net.gen_qmin)
    gen_in_service = list(net.gen_in_service)

    for i in range(len(gen_buses)):
        if not gen_in_service[i]:
            continue
        eid = f"gen:{gen_buses[i]}/{gen_ids[i]}"

        if gen_p[i] > gen_pmax[i] + 0.01:
            issues.append(
                ModelIssue(
                    "warning",
                    "generation",
                    f"Pg = {gen_p[i]:.1f} MW > Pmax = {gen_pmax[i]:.1f} MW",
                    "generator",
                    eid,
                )
            )
        if gen_p[i] < gen_pmin[i] - 0.01:
            issues.append(
                ModelIssue(
                    "warning",
                    "generation",
                    f"Pg = {gen_p[i]:.1f} MW < Pmin = {gen_pmin[i]:.1f} MW",
                    "generator",
                    eid,
                )
            )
        if gen_qmax[i] < gen_qmin[i] - 0.01:
            issues.append(
                ModelIssue(
                    "error",
                    "generation",
                    f"Qmax = {gen_qmax[i]:.1f} < Qmin = {gen_qmin[i]:.1f} Mvar",
                    "generator",
                    eid,
                )
            )

    # System-wide: total gen < total load?
    total_gen = net.total_generation_mw
    total_load = net.total_load_mw
    if total_gen > 0 and total_load > 0 and total_gen < total_load * 0.95:
        issues.append(
            ModelIssue(
                "warning",
                "generation",
                f"Total scheduled gen ({total_gen:.0f} MW) < 95% of total load ({total_load:.0f} MW)",
                "bus",
                "system",
            )
        )

    return issues


def _check_ratings(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    from_buses = list(net.branch_from)
    to_buses = list(net.branch_to)
    circuits = list(net.branch_circuit)
    rate_a = list(net.branch_rate_a)
    rate_b = list(net.branch_rate_b)
    rate_c = list(net.branch_rate_c)

    for i in range(len(from_buses)):
        eid = f"branch:{from_buses[i]}→{to_buses[i]}({circuits[i]})"
        if rate_a[i] == 0:
            issues.append(
                ModelIssue(
                    "info",
                    "ratings",
                    "Missing thermal rating (Rate A = 0)",
                    "branch",
                    eid,
                )
            )
        if rate_b[i] > 0 and rate_a[i] > 0 and rate_b[i] < rate_a[i]:
            issues.append(
                ModelIssue(
                    "warning",
                    "ratings",
                    f"Rate B ({rate_b[i]:.0f}) < Rate A ({rate_a[i]:.0f}) — inverted emergency rating",
                    "branch",
                    eid,
                )
            )

    return issues


def _check_loads(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    bus_nums = list(net.bus_numbers)
    bus_pd = list(net.bus_pd)
    total_load = net.total_load_mw

    for i in range(len(bus_nums)):
        pd = bus_pd[i]
        if pd < -0.01:
            issues.append(
                ModelIssue(
                    "info",
                    "load",
                    f"Negative load Pd = {pd:.1f} MW (net generation at load bus?)",
                    "bus",
                    f"bus:{bus_nums[i]}",
                )
            )
        if total_load > 0 and pd > total_load * 0.20:
            issues.append(
                ModelIssue(
                    "warning",
                    "load",
                    f"Single-bus load Pd = {pd:.0f} MW is >20% of total system load ({total_load:.0f} MW)",
                    "bus",
                    f"bus:{bus_nums[i]}",
                )
            )

    return issues


def _check_transformers(net: Network) -> list[ModelIssue]:
    issues: list[ModelIssue] = []
    for br in net.transformers:
        eid = f"branch:{br.from_bus}→{br.to_bus}({br.circuit})"
        tap = br.tap
        if tap < 0.8 or tap > 1.2:
            issues.append(
                ModelIssue(
                    "warning",
                    "transformer",
                    f"Tap ratio = {tap:.4f} outside [0.8, 1.2]",
                    "branch",
                    eid,
                )
            )
        shift = br.shift_deg
        if abs(shift) > 60:
            issues.append(
                ModelIssue(
                    "warning",
                    "transformer",
                    f"Phase shift = {shift:.1f}° exceeds 60°",
                    "branch",
                    eid,
                )
            )

    return issues
