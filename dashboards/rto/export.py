# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Settlement extraction for the RTO dashboard.

Turns a goc3 native dispatch result into the day-ahead settlement
view: per-bus LMPs (energy / congestion / loss components),
per-resource energy awards and payments, per-zone AS awards /
clearing prices / payments, and system totals.

The dispatch result can be either a typed :class:`DispatchResult`
or the dict shape ``surge.market.go_c3.solve_workflow`` returns —
this module reads via ``getattr`` with dict fallbacks so it works
on both.
"""

from __future__ import annotations

from typing import Any


def extract_settlement(
    dispatch_result: Any,
    period_durations_hours: list[float],
) -> dict[str, Any]:
    """Build the day-ahead settlement dict from a solved dispatch."""
    periods = list(_periods(dispatch_result))
    n_periods = len(periods)
    durations = list(period_durations_hours)
    if len(durations) != n_periods:
        raise ValueError(
            f"period_durations_hours length ({len(durations)}) != "
            f"dispatch_result.periods length ({n_periods})"
        )

    # -- LMPs per bus per period --
    lmps_per_period: list[dict[str, Any]] = []
    for t, period in enumerate(periods):
        lmps_per_period.append(
            {
                "period": t,
                "duration_hours": durations[t],
                "buses": [
                    {
                        "bus_number": b["bus_number"],
                        "lmp": b.get("lmp"),
                        "mec": b.get("mec"),
                        "mcc": b.get("mcc"),
                        "mlc": b.get("mlc"),
                        # Reactive marginal price ($/MVAr-h) — populated
                        # only on AC SCED solves; ``None`` otherwise.
                        "q_lmp": b.get("q_lmp"),
                        "voltage_pu": b.get("voltage_pu"),
                        "net_injection_mw": b.get("net_injection_mw"),
                        "withdrawals_mw": b.get("withdrawals_mw"),
                        # Reactive injection / withdrawal (MVAr) for
                        # the loads-tab reactive view.
                        "net_reactive_injection_mvar": b.get("net_reactive_injection_mvar"),
                        "withdrawals_mvar": b.get("withdrawals_mvar"),
                    }
                    for b in period["bus_results"]
                ],
            }
        )

    # -- Energy awards + payments per resource per period --
    energy_rows: list[dict[str, Any]] = []
    total_energy_payment = 0.0
    # ``reserve_awards_by_resource[rid][pid][t] = award_mw``
    reserve_awards_by_resource: dict[str, dict[str, list[float]]] = {}
    for t, period in enumerate(periods):
        bus_lmp = {b["bus_number"]: (b.get("lmp") or 0.0) for b in period["bus_results"]}
        duration = durations[t]
        for res in period["resource_results"]:
            if res.get("kind") != "generator":
                continue
            power_mw = float(res.get("power_mw", 0.0) or 0.0)
            energy_mwh = power_mw * duration
            lmp = float(bus_lmp.get(res["bus_number"], 0.0))
            payment = energy_mwh * lmp
            total_energy_payment += payment
            energy_rows.append(
                {
                    "period": t,
                    "resource_id": res["resource_id"],
                    "bus_number": res["bus_number"],
                    "power_mw": power_mw,
                    "energy_mwh": energy_mwh,
                    "lmp": lmp,
                    "payment_dollars": payment,
                    "energy_cost_dollars": float(res.get("energy_cost", 0.0) or 0.0),
                }
            )
            rid = res["resource_id"]
            for pid, award_mw in (res.get("reserve_awards") or {}).items():
                if abs(float(award_mw or 0.0)) < 1e-9:
                    continue
                rec = reserve_awards_by_resource.setdefault(rid, {}).setdefault(
                    pid, [0.0] * n_periods
                )
                rec[t] = float(award_mw or 0.0)

    # -- AS awards + clearing prices per zone per product per period --
    as_rows: list[dict[str, Any]] = []
    total_as_payment = 0.0
    total_shortfall_penalty = 0.0
    as_by_zone_product_period: dict[tuple[int, str, int], dict[str, Any]] = {}
    for t, period in enumerate(periods):
        duration = durations[t]
        for rr in period.get("reserve_results", []):
            if rr.get("scope") != "zone":
                continue
            zone_id = int(rr.get("zone_id"))
            product_id = str(rr.get("product_id"))
            provided_mw = float(rr.get("provided_mw", 0.0) or 0.0)
            requirement_mw = float(rr.get("requirement_mw", 0.0) or 0.0)
            shortfall_mw = float(rr.get("shortfall_mw", 0.0) or 0.0)
            clearing_price = float(rr.get("clearing_price", 0.0) or 0.0)
            shortfall_cost = float(rr.get("shortfall_cost", 0.0) or 0.0)
            as_payment = provided_mw * clearing_price * duration
            total_as_payment += as_payment
            total_shortfall_penalty += shortfall_cost
            as_by_zone_product_period[(zone_id, product_id, t)] = {
                "zone_id": zone_id,
                "product_id": product_id,
                "period": t,
                "requirement_mw": requirement_mw,
                "provided_mw": provided_mw,
                "shortfall_mw": shortfall_mw,
                "clearing_price": clearing_price,
                "payment_dollars": as_payment,
                "shortfall_cost_dollars": shortfall_cost,
            }
            as_rows.append(as_by_zone_product_period[(zone_id, product_id, t)])

    # -- Per-resource AS award + revenue with carrying mask --
    clearing_price_for: dict[tuple[str, int], float] = {}
    for (zone_id, pid, t), row in as_by_zone_product_period.items():
        clearing_price_for.setdefault((pid, t), float(row["clearing_price"]))

    resource_reserve_rows: list[dict[str, Any]] = []
    for rid, by_product in reserve_awards_by_resource.items():
        rec: dict[str, Any] = {
            "resource_id": rid,
            "by_product": {},
            "carrying_mask": [False] * n_periods,
            "total_revenue_dollars": 0.0,
        }
        for pid, awards_per_period in by_product.items():
            revenue_per_period: list[float] = []
            for t, award in enumerate(awards_per_period):
                price = clearing_price_for.get((pid, t), 0.0)
                rev = float(award) * price * durations[t]
                revenue_per_period.append(rev)
                rec["total_revenue_dollars"] += rev
                if abs(award) > 1e-9:
                    rec["carrying_mask"][t] = True
            rec["by_product"][pid] = {
                "award_mw": list(awards_per_period),
                "revenue_dollars": revenue_per_period,
            }
        resource_reserve_rows.append(rec)

    # -- Load payment per bus per period; congestion rent = load payment − gen revenue --
    total_load_payment = 0.0
    for t, period in enumerate(periods):
        duration = durations[t]
        for b in period["bus_results"]:
            lmp = float(b.get("lmp") or 0.0)
            withdrawal_mw = float(b.get("withdrawals_mw") or 0.0)
            total_load_payment += lmp * withdrawal_mw * duration
    congestion_rent = total_load_payment - total_energy_payment

    summary = _summary(dispatch_result)
    totals = {
        "energy_payment_dollars": total_energy_payment,
        "load_payment_dollars": total_load_payment,
        "as_payment_dollars": total_as_payment,
        "shortfall_penalty_dollars": total_shortfall_penalty,
        "congestion_rent_dollars": congestion_rent,
        "production_cost_dollars": float(summary.get("total_cost", 0.0) or 0.0),
    }

    return {
        "periods": n_periods,
        "period_durations_hours": list(durations),
        "lmps_per_period": lmps_per_period,
        "energy_awards": energy_rows,
        "as_awards": as_rows,
        "resource_reserve_awards": resource_reserve_rows,
        "totals": totals,
    }


def _periods(dispatch_result: Any) -> list[dict[str, Any]]:
    """Read ``periods`` whether the result is a typed object or a dict."""
    if isinstance(dispatch_result, dict):
        return list(dispatch_result.get("periods") or [])
    return list(getattr(dispatch_result, "periods", []) or [])


def _summary(dispatch_result: Any) -> dict[str, Any]:
    if isinstance(dispatch_result, dict):
        return dict(dispatch_result.get("summary") or {})
    return dict(getattr(dispatch_result, "summary", {}) or {})


__all__ = ["extract_settlement"]
