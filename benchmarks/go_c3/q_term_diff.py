# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Per-term Q (and P) bus-residual diff between two GO C3 bus_detail payloads.

Consumes the JSON payloads produced by
``markets/go_c3/validator.py::extract_bus_residuals_with_official_tool``
(which embed ``bus_detail[bus][q_terms_pu][term][period]`` for each of
``sd, sh, acl_fr, acl_to, dcl_fr, dcl_to, xfr_fr, xfr_to``) and reports
where the surge solution diverges from a reference solution at the level
of individual injection terms.

The point of this diagnostic is to answer "is our wrong term ``sd_q``,
``sh_q``, ``acl_q``, ``dcl_q``, or ``xfr_q``?" without re-running surge.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
from typing import Any, Iterable


_TERMS = ("sd", "sh", "acl_fr", "acl_to", "dcl_fr", "dcl_to", "xfr_fr", "xfr_to")


@dataclass(frozen=True)
class _BusPeriodDelta:
    bus_uid: str
    period: int
    lhs_q_residual_pu: float
    rhs_q_residual_pu: float
    q_residual_delta_pu: float
    lhs_p_residual_pu: float
    rhs_p_residual_pu: float
    p_residual_delta_pu: float
    lhs_vm_pu: float
    rhs_vm_pu: float
    vm_delta_pu: float
    lhs_va_rad: float
    rhs_va_rad: float
    va_delta_rad: float
    q_term_deltas_pu: dict[str, float]
    p_term_deltas_pu: dict[str, float]
    lhs_q_terms_pu: dict[str, float]
    rhs_q_terms_pu: dict[str, float]
    lhs_p_terms_pu: dict[str, float]
    rhs_p_terms_pu: dict[str, float]


def _load_bus_detail(payload_path: Path) -> dict[str, Any]:
    payload = json.loads(payload_path.read_text(encoding="utf-8"))
    bus_detail = payload.get("bus_detail")
    if not isinstance(bus_detail, dict):
        raise ValueError(
            f"{payload_path} is not a bus-residuals payload "
            "(missing top-level 'bus_detail')"
        )
    return bus_detail


def _bus_period_count(bus_detail: dict[str, Any]) -> int:
    for bus_record in bus_detail.values():
        q_shortfall = bus_record.get("q_shortfall_pu", [])
        if isinstance(q_shortfall, list):
            return len(q_shortfall)
    return 0


def _safe_value(series: Any, period: int) -> float:
    if isinstance(series, list) and 0 <= period < len(series):
        return float(series[period])
    return 0.0


def _term_value(record: dict[str, Any], terms_key: str, term: str, period: int) -> float:
    terms = record.get(terms_key, {})
    if not isinstance(terms, dict):
        return 0.0
    return _safe_value(terms.get(term), period)


def _build_period_delta(
    bus_uid: str,
    period: int,
    lhs_record: dict[str, Any],
    rhs_record: dict[str, Any],
) -> _BusPeriodDelta:
    lhs_q_residual = _safe_value(lhs_record.get("q_shortfall_pu"), period)
    rhs_q_residual = _safe_value(rhs_record.get("q_shortfall_pu"), period)
    lhs_p_residual = _safe_value(lhs_record.get("p_shortfall_pu"), period)
    rhs_p_residual = _safe_value(rhs_record.get("p_shortfall_pu"), period)
    lhs_vm = _safe_value(lhs_record.get("vm_pu"), period)
    rhs_vm = _safe_value(rhs_record.get("vm_pu"), period)
    lhs_va = _safe_value(lhs_record.get("va_rad"), period)
    rhs_va = _safe_value(rhs_record.get("va_rad"), period)

    lhs_q_terms = {term: _term_value(lhs_record, "q_terms_pu", term, period) for term in _TERMS}
    rhs_q_terms = {term: _term_value(rhs_record, "q_terms_pu", term, period) for term in _TERMS}
    lhs_p_terms = {term: _term_value(lhs_record, "p_terms_pu", term, period) for term in _TERMS}
    rhs_p_terms = {term: _term_value(rhs_record, "p_terms_pu", term, period) for term in _TERMS}

    q_term_deltas = {term: lhs_q_terms[term] - rhs_q_terms[term] for term in _TERMS}
    p_term_deltas = {term: lhs_p_terms[term] - rhs_p_terms[term] for term in _TERMS}

    return _BusPeriodDelta(
        bus_uid=bus_uid,
        period=period,
        lhs_q_residual_pu=lhs_q_residual,
        rhs_q_residual_pu=rhs_q_residual,
        q_residual_delta_pu=lhs_q_residual - rhs_q_residual,
        lhs_p_residual_pu=lhs_p_residual,
        rhs_p_residual_pu=rhs_p_residual,
        p_residual_delta_pu=lhs_p_residual - rhs_p_residual,
        lhs_vm_pu=lhs_vm,
        rhs_vm_pu=rhs_vm,
        vm_delta_pu=lhs_vm - rhs_vm,
        lhs_va_rad=lhs_va,
        rhs_va_rad=rhs_va,
        va_delta_rad=lhs_va - rhs_va,
        q_term_deltas_pu=q_term_deltas,
        p_term_deltas_pu=p_term_deltas,
        lhs_q_terms_pu=lhs_q_terms,
        rhs_q_terms_pu=rhs_q_terms,
        lhs_p_terms_pu=lhs_p_terms,
        rhs_p_terms_pu=rhs_p_terms,
    )


def _delta_to_dict(delta: _BusPeriodDelta) -> dict[str, Any]:
    return {
        "bus_uid": delta.bus_uid,
        "period": delta.period,
        "q": {
            "lhs_residual_pu": delta.lhs_q_residual_pu,
            "rhs_residual_pu": delta.rhs_q_residual_pu,
            "residual_delta_pu": delta.q_residual_delta_pu,
            "term_deltas_pu": dict(delta.q_term_deltas_pu),
            "lhs_terms_pu": dict(delta.lhs_q_terms_pu),
            "rhs_terms_pu": dict(delta.rhs_q_terms_pu),
        },
        "p": {
            "lhs_residual_pu": delta.lhs_p_residual_pu,
            "rhs_residual_pu": delta.rhs_p_residual_pu,
            "residual_delta_pu": delta.p_residual_delta_pu,
            "term_deltas_pu": dict(delta.p_term_deltas_pu),
            "lhs_terms_pu": dict(delta.lhs_p_terms_pu),
            "rhs_terms_pu": dict(delta.rhs_p_terms_pu),
        },
        "voltage": {
            "lhs_vm_pu": delta.lhs_vm_pu,
            "rhs_vm_pu": delta.rhs_vm_pu,
            "vm_delta_pu": delta.vm_delta_pu,
            "lhs_va_rad": delta.lhs_va_rad,
            "rhs_va_rad": delta.rhs_va_rad,
            "va_delta_rad": delta.va_delta_rad,
        },
    }


def diff_bus_residuals(
    lhs_payload_path: Path,
    rhs_payload_path: Path,
    *,
    top_k: int = 20,
) -> dict[str, Any]:
    """Compute per-bus per-period Q/P term diffs between two bus_detail payloads.

    ``lhs`` is the *probe* (typically surge); ``rhs`` is the *reference*
    (typically the benchmark winner).
    """
    lhs_bus_detail = _load_bus_detail(lhs_payload_path)
    rhs_bus_detail = _load_bus_detail(rhs_payload_path)

    bus_uids = sorted(set(lhs_bus_detail) | set(rhs_bus_detail))
    period_count = max(_bus_period_count(lhs_bus_detail), _bus_period_count(rhs_bus_detail))

    deltas: list[_BusPeriodDelta] = []
    bus_summaries: list[dict[str, Any]] = []
    cumulative_q_term_abs: dict[str, float] = {term: 0.0 for term in _TERMS}
    cumulative_p_term_abs: dict[str, float] = {term: 0.0 for term in _TERMS}

    for bus_uid in bus_uids:
        lhs_record = lhs_bus_detail.get(bus_uid, {})
        rhs_record = rhs_bus_detail.get(bus_uid, {})
        bus_max_q_residual_delta = 0.0
        bus_sum_abs_q_residual_delta = 0.0
        bus_max_vm_delta = 0.0
        worst_period = 0
        for period in range(period_count):
            delta = _build_period_delta(bus_uid, period, lhs_record, rhs_record)
            deltas.append(delta)
            for term in _TERMS:
                cumulative_q_term_abs[term] += abs(delta.q_term_deltas_pu[term])
                cumulative_p_term_abs[term] += abs(delta.p_term_deltas_pu[term])
            abs_q_delta = abs(delta.q_residual_delta_pu)
            bus_sum_abs_q_residual_delta += abs_q_delta
            if abs_q_delta > bus_max_q_residual_delta:
                bus_max_q_residual_delta = abs_q_delta
                worst_period = period
            if abs(delta.vm_delta_pu) > bus_max_vm_delta:
                bus_max_vm_delta = abs(delta.vm_delta_pu)
        bus_summaries.append(
            {
                "bus_uid": bus_uid,
                "max_abs_q_residual_delta_pu": bus_max_q_residual_delta,
                "sum_abs_q_residual_delta_pu": bus_sum_abs_q_residual_delta,
                "max_abs_vm_delta_pu": bus_max_vm_delta,
                "worst_period": worst_period,
            }
        )

    bus_summaries.sort(key=lambda item: -float(item["max_abs_q_residual_delta_pu"]))
    deltas.sort(key=lambda item: -abs(item.q_residual_delta_pu))
    top_q_deltas = [_delta_to_dict(delta) for delta in deltas[:top_k]]

    return {
        "lhs_payload_path": str(lhs_payload_path.expanduser().resolve()),
        "rhs_payload_path": str(rhs_payload_path.expanduser().resolve()),
        "bus_count": len(bus_uids),
        "period_count": period_count,
        "top_k": int(top_k),
        "cumulative_abs_q_term_deltas_pu": cumulative_q_term_abs,
        "cumulative_abs_p_term_deltas_pu": cumulative_p_term_abs,
        "bus_summaries": bus_summaries[: max(top_k, 20)],
        "top_q_residual_deltas": top_q_deltas,
    }


def _format_term_breakdown(term_deltas: dict[str, float]) -> str:
    items = sorted(term_deltas.items(), key=lambda kv: -abs(kv[1]))
    return ", ".join(f"{name}={value:+.4f}" for name, value in items if abs(value) > 1e-9) or "(none)"


def format_diff_report(report: dict[str, Any], *, top_k: int = 20) -> list[str]:
    lines = [
        f"q-term-diff: {report['lhs_payload_path']}",
        f"          vs {report['rhs_payload_path']}",
        f"buses={report['bus_count']} periods={report['period_count']}",
        "",
        "cumulative |Δ| over all (bus,period) [pu]:",
        "  q-terms: " + _format_term_breakdown(report["cumulative_abs_q_term_deltas_pu"]),
        "  p-terms: " + _format_term_breakdown(report["cumulative_abs_p_term_deltas_pu"]),
        "",
        "top buses by max |Δq_residual|:",
    ]
    for entry in report["bus_summaries"][:top_k]:
        lines.append(
            "  bus={bus} max|Δq|={mq:.4f}pu Σ|Δq|={sq:.4f}pu max|Δvm|={mv:.4f}pu worst_t={t}".format(
                bus=entry["bus_uid"],
                mq=entry["max_abs_q_residual_delta_pu"],
                sq=entry["sum_abs_q_residual_delta_pu"],
                mv=entry["max_abs_vm_delta_pu"],
                t=entry["worst_period"],
            )
        )
    lines.append("")
    lines.append("top (bus,period) Δq_residual breakdown:")
    for entry in report["top_q_residual_deltas"][:top_k]:
        q = entry["q"]
        v = entry["voltage"]
        lines.append(
            "  bus={bus} t={t} Δq={dq:+.4f}pu (lhs={lq:+.4f} rhs={rq:+.4f}) "
            "Δvm={dvm:+.4f}pu lhs_vm={lvm:.4f} rhs_vm={rvm:.4f}".format(
                bus=entry["bus_uid"],
                t=entry["period"],
                dq=q["residual_delta_pu"],
                lq=q["lhs_residual_pu"],
                rq=q["rhs_residual_pu"],
                dvm=v["vm_delta_pu"],
                lvm=v["lhs_vm_pu"],
                rvm=v["rhs_vm_pu"],
            )
        )
        lines.append("    q term Δ: " + _format_term_breakdown(q["term_deltas_pu"]))
    return lines


def write_diff_csv(report: dict[str, Any], csv_path: Path) -> None:
    headers = (
        ["bus_uid", "period", "q_residual_delta_pu", "lhs_q_residual_pu", "rhs_q_residual_pu",
         "vm_delta_pu", "lhs_vm_pu", "rhs_vm_pu", "va_delta_rad"]
        + [f"q_{term}_delta_pu" for term in _TERMS]
        + [f"q_{term}_lhs_pu" for term in _TERMS]
        + [f"q_{term}_rhs_pu" for term in _TERMS]
    )
    rows = [",".join(headers)]
    for entry in report["top_q_residual_deltas"]:
        q = entry["q"]
        v = entry["voltage"]
        row = [
            entry["bus_uid"],
            str(entry["period"]),
            f"{q['residual_delta_pu']:.6f}",
            f"{q['lhs_residual_pu']:.6f}",
            f"{q['rhs_residual_pu']:.6f}",
            f"{v['vm_delta_pu']:.6f}",
            f"{v['lhs_vm_pu']:.6f}",
            f"{v['rhs_vm_pu']:.6f}",
            f"{v['va_delta_rad']:.6f}",
        ]
        for term in _TERMS:
            row.append(f"{q['term_deltas_pu'][term]:.6f}")
        for term in _TERMS:
            row.append(f"{q['lhs_terms_pu'][term]:.6f}")
        for term in _TERMS:
            row.append(f"{q['rhs_terms_pu'][term]:.6f}")
        rows.append(",".join(row))
    csv_path.write_text("\n".join(rows) + "\n", encoding="utf-8")
