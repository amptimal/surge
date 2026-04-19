# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Settlement extraction from a native :class:`DispatchResult`.

Turns the solver's native result into the day-ahead settlement view:
per-bus LMPs (energy / congestion / loss components), per-resource
energy awards and payments, per-zone AS awards / clearing prices /
payments, and system totals.
"""

from __future__ import annotations

from typing import Any

from .problem import RtoProblem


def extract_settlement(dispatch_result: Any, problem: RtoProblem) -> dict[str, Any]:
    """Build the day-ahead settlement dict."""
    periods = list(dispatch_result.periods)
    n_periods = len(periods)
    durations = problem.period_durations_hours
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
                        "net_injection_mw": b.get("net_injection_mw"),
                        "withdrawals_mw": b.get("withdrawals_mw"),
                    }
                    for b in period["bus_results"]
                ],
            }
        )

    # -- Energy awards + payments per resource per period --
    energy_rows: list[dict[str, Any]] = []
    total_energy_payment = 0.0
    for t, period in enumerate(periods):
        # bus_number → LMP lookup
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
            # AS payment = provided_mw × clearing_price × duration.
            # Procurement cost (supply-side) comes from reserve offers
            # and is already folded into the dispatch solution's total
            # cost — what we report here is the market-side clearing.
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

    # -- Load payment per bus per period; congestion rent = load payment − gen revenue --
    # On a lossless DC clearing: congestion rent = Σ LMP × D − Σ LMP × G, where
    # ``withdrawals_mw`` IS the demand D at the bus. Gen revenue is the
    # ``total_energy_payment`` already accumulated above.
    total_load_payment = 0.0
    for t, period in enumerate(periods):
        duration = durations[t]
        for b in period["bus_results"]:
            lmp = float(b.get("lmp") or 0.0)
            withdrawal_mw = float(b.get("withdrawals_mw") or 0.0)
            total_load_payment += lmp * withdrawal_mw * duration
    congestion_rent = total_load_payment - total_energy_payment

    totals = {
        "energy_payment_dollars": total_energy_payment,
        "load_payment_dollars": total_load_payment,
        "as_payment_dollars": total_as_payment,
        "shortfall_penalty_dollars": total_shortfall_penalty,
        "congestion_rent_dollars": congestion_rent,
        "production_cost_dollars": float(
            dispatch_result.summary.get("total_cost", 0.0) or 0.0
        ),
    }

    return {
        "periods": n_periods,
        "period_durations_hours": list(durations),
        "lmps_per_period": lmps_per_period,
        "energy_awards": energy_rows,
        "as_awards": as_rows,
        "totals": totals,
    }


__all__ = ["extract_settlement"]
