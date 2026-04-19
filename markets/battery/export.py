# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Revenue P&L + SOC trajectory + cycle-count report."""

from __future__ import annotations

from typing import Any

from .problem import BatteryProblem


def extract_revenue_report(
    dispatch_result: Any, problem: BatteryProblem
) -> dict[str, Any]:
    """Revenue report for a coupled (single :class:`DispatchResult`) solve."""
    periods = list(dispatch_result.periods)
    if len(periods) != problem.periods:
        raise ValueError(
            f"dispatch_result has {len(periods)} periods; problem has {problem.periods}"
        )
    return _extract_from_period_dicts(periods, problem)


def extract_revenue_report_from_sequence(
    per_period_results: list[Any], problem: BatteryProblem
) -> dict[str, Any]:
    """Revenue report for a sequential solve (list of single-period results).

    Each entry in ``per_period_results`` is the
    :class:`DispatchResult` returned by one single-period solve. We
    concatenate each result's first (and only) period into a single
    list and pass it through the shared extraction logic.
    """
    if len(per_period_results) != problem.periods:
        raise ValueError(
            f"sequential result count ({len(per_period_results)}) does not match "
            f"problem.periods ({problem.periods})"
        )
    # Each single-period solve's .periods list has exactly one entry.
    period_dicts: list[dict[str, Any]] = []
    for i, r in enumerate(per_period_results):
        if len(r.periods) != 1:
            raise ValueError(
                f"sequential result #{i} has {len(r.periods)} periods, expected 1"
            )
        period_dicts.append(r.periods[0])
    return _extract_from_period_dicts(period_dicts, problem)


def _extract_from_period_dicts(
    periods: list[dict[str, Any]], problem: BatteryProblem
) -> dict[str, Any]:
    """Shared extraction used by both coupled and sequential paths.

    ``periods`` is a list of per-period dicts in the shape
    :class:`DispatchResult.periods` entries take.
    """
    n_periods = len(periods)
    durations = problem.period_durations_hours
    lmps = problem.lmp_forecast_per_mwh
    if len(durations) != n_periods:
        raise ValueError(
            f"period_durations_hours ({len(durations)}) does not match "
            f"period_dicts length ({n_periods})"
        )

    as_price_by_product = {
        ap.product_def.id: ap.price_forecast_per_mwh for ap in problem.as_products
    }

    schedule: list[dict[str, Any]] = []
    as_breakdown: list[dict[str, Any]] = []
    total_energy_revenue = 0.0
    total_degradation_cost = 0.0
    total_charge_mwh = 0.0
    total_discharge_mwh = 0.0
    total_as_revenue = 0.0

    for t, period in enumerate(periods):
        duration = durations[t]
        lmp = float(lmps[t])

        bess_entry: dict[str, Any] | None = None
        for res in period["resource_results"]:
            if res.get("resource_id") == problem.BESS_RESOURCE_ID:
                det = res.get("detail", {}) or {}
                charge_mw = float(det.get("charge_mw", 0.0) or 0.0)
                discharge_mw = float(det.get("discharge_mw", 0.0) or 0.0)
                soc_mwh = float(det.get("soc_mwh", 0.0) or 0.0)
                net_export_mw = float(res.get("power_mw", 0.0) or 0.0)
                charge_mwh = charge_mw * duration
                discharge_mwh = discharge_mw * duration
                throughput_mwh = charge_mwh + discharge_mwh
                energy_revenue = net_export_mw * lmp * duration
                degradation_cost = (
                    problem.site.bess_degradation_cost_per_mwh * throughput_mwh
                )
                total_energy_revenue += energy_revenue
                total_degradation_cost += degradation_cost
                total_charge_mwh += charge_mwh
                total_discharge_mwh += discharge_mwh

                # AS awards: {product_id: mw} dict on the resource result.
                period_as: list[dict[str, Any]] = []
                awards = res.get("reserve_awards") or det.get("reserve_awards") or {}
                if isinstance(awards, dict):
                    for product_id, award_mw in awards.items():
                        if product_id is None or award_mw is None:
                            continue
                        award_mw = float(award_mw)
                        if abs(award_mw) < 1e-9:
                            continue
                        price = float(
                            as_price_by_product.get(
                                product_id, [0.0] * n_periods
                            )[t]
                        )
                        as_revenue = award_mw * price * duration
                        total_as_revenue += as_revenue
                        period_as.append(
                            {
                                "product_id": product_id,
                                "award_mw": award_mw,
                                "price_per_mwh": price,
                                "revenue_dollars": as_revenue,
                            }
                        )
                as_breakdown.append({"period": t, "awards": period_as})

                bess_entry = {
                    "period": t,
                    "duration_hours": duration,
                    "lmp": lmp,
                    "charge_mw": charge_mw,
                    "discharge_mw": discharge_mw,
                    "net_export_mw": net_export_mw,
                    "soc_mwh": soc_mwh,
                    "energy_revenue_dollars": energy_revenue,
                    "degradation_cost_dollars": degradation_cost,
                }
                break

        if bess_entry is None:
            bess_entry = {
                "period": t,
                "duration_hours": duration,
                "lmp": lmp,
                "charge_mw": 0.0,
                "discharge_mw": 0.0,
                "net_export_mw": 0.0,
                "soc_mwh": 0.0,
                "energy_revenue_dollars": 0.0,
                "degradation_cost_dollars": 0.0,
            }
        schedule.append(bess_entry)

    energy_capacity = problem.site.bess_energy_mwh
    total_throughput_mwh = total_charge_mwh + total_discharge_mwh
    full_equivalent_cycles = (
        total_throughput_mwh / (2.0 * energy_capacity)
        if energy_capacity > 0
        else 0.0
    )

    totals = {
        "energy_revenue_dollars": total_energy_revenue,
        "as_revenue_dollars": total_as_revenue,
        "degradation_cost_dollars": total_degradation_cost,
        "net_revenue_dollars": total_energy_revenue
        + total_as_revenue
        - total_degradation_cost,
        "total_charge_mwh": total_charge_mwh,
        "total_discharge_mwh": total_discharge_mwh,
        "total_throughput_mwh": total_throughput_mwh,
        "full_equivalent_cycles": full_equivalent_cycles,
    }

    return {
        "periods": n_periods,
        "period_durations_hours": list(durations),
        "schedule": schedule,
        "as_breakdown": as_breakdown,
        "totals": totals,
    }


__all__ = ["extract_revenue_report", "extract_revenue_report_from_sequence"]
