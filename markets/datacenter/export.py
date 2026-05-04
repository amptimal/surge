# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Per-resource P&L report for the datacenter operator.

Produces a ``pnl-report.json`` payload with:

* Per-period schedule: LMP, IT load served / curtailed by tier, each
  on-site resource's MW, BESS SOC, grid import / export, AS awards.
* Per-resource totals: revenue, cost, capacity factor, fuel cost,
  CO₂ tonnes, startups.
* Top-line totals: gross compute revenue, energy import cost, energy
  export revenue, AS revenue, fuel cost, BESS degradation, transmission
  demand charge, net P&L.
"""

from __future__ import annotations

from typing import Any

from .problem import (
    BESS_RESOURCE_ID,
    DataCenterProblem,
    GRID_EXPORT_RESOURCE_ID,
    GRID_IMPORT_RESOURCE_ID,
    SOLAR_RESOURCE_ID,
    WIND_RESOURCE_ID,
)


def extract_pl_report(dispatch_result: Any, problem: DataCenterProblem) -> dict[str, Any]:
    periods = list(dispatch_result.periods)
    if len(periods) != problem.periods:
        raise ValueError(
            f"dispatch_result has {len(periods)} periods; problem has {problem.periods}"
        )
    return _extract_from_period_dicts(periods, problem)


def extract_pl_report_from_sequence(
    per_period_results: list[Any], problem: DataCenterProblem
) -> dict[str, Any]:
    if len(per_period_results) != problem.periods:
        raise ValueError(
            f"sequential result count ({len(per_period_results)}) does not match "
            f"problem.periods ({problem.periods})"
        )
    period_dicts: list[dict[str, Any]] = []
    for i, r in enumerate(per_period_results):
        if len(r.periods) != 1:
            raise ValueError(
                f"sequential result #{i} has {len(r.periods)} periods, expected 1"
            )
        period_dicts.append(r.periods[0])
    return _extract_from_period_dicts(period_dicts, problem)


def _extract_from_period_dicts(
    periods: list[dict[str, Any]], problem: DataCenterProblem
) -> dict[str, Any]:
    n = len(periods)
    durations = problem.period_durations_hours
    lmps = problem.lmp_forecast_per_mwh

    # Resource-id buckets we'll match the dispatch result against.
    thermal_ids: dict[str, str] = {}
    for slot, thermal in problem.thermal_specs():
        if thermal is None:
            continue
        thermal_ids[thermal.resource_id] = slot

    nuclear_id = problem.site.nuclear.resource_id if problem.site.nuclear else None
    tier_resource_ids = {
        problem.curtailable_load_resource_id(t.tier_id): t for t in problem.site.it_load.tiers
    }

    schedule: list[dict[str, Any]] = []
    totals: dict[str, float] = {
        "compute_revenue_dollars": 0.0,
        "compute_curtailment_loss_dollars": 0.0,
        "energy_import_cost_dollars": 0.0,
        "energy_export_revenue_dollars": 0.0,
        "as_revenue_dollars": 0.0,
        "fuel_cost_dollars": 0.0,
        "vom_cost_dollars": 0.0,
        "no_load_cost_dollars": 0.0,
        "startup_cost_dollars": 0.0,
        "bess_degradation_cost_dollars": 0.0,
        "tx_demand_charge_dollars": 0.0,
        "co2_tonnes": 0.0,
        "total_charge_mwh": 0.0,
        "total_discharge_mwh": 0.0,
        "throughput_mwh": 0.0,
        "must_serve_mwh": 0.0,
        "tier_served_mwh": 0.0,
        "tier_curtailed_mwh": 0.0,
        "grid_import_mwh": 0.0,
        "grid_export_mwh": 0.0,
        "renewables_mwh": 0.0,
        "thermal_mwh": 0.0,
        "nuclear_mwh": 0.0,
        # REC revenue earned across the horizon — solar + wind
        # generation × their respective rec_value_per_mwh. Driven by
        # the SCUC's negative-cost offer (see SolarSpec /
        # WindSpec.rec_value_per_mwh) so this is consistent with the
        # LP's objective.
        "rec_revenue_dollars": 0.0,
    }

    as_price_by_product = {
        ap.product_def.id: ap.price_forecast_per_mwh for ap in problem.as_products
    }

    grid_import_by_period: list[float] = [0.0] * n

    for t, period in enumerate(periods):
        dt = float(durations[t])
        lmp_t = float(lmps[t])
        must_serve_mw = float(problem.site.it_load.must_serve_mw[t])

        entry: dict[str, Any] = {
            "period": t,
            "duration_hours": dt,
            "lmp": lmp_t,
            "must_serve_mw": must_serve_mw,
            "tiers": [],
            "renewables": {"solar_mw": 0.0, "wind_mw": 0.0},
            "bess": {"charge_mw": 0.0, "discharge_mw": 0.0, "soc_mwh": 0.0},
            "thermals": {},
            "nuclear_mw": 0.0,
            "grid_import_mw": 0.0,
            "grid_export_mw": 0.0,
            "as_awards": {},
        }
        totals["must_serve_mwh"] += must_serve_mw * dt

        for res in period.get("resource_results", []):
            rid = res.get("resource_id")
            mw = float(res.get("power_mw", 0.0) or 0.0)
            detail = res.get("detail") or {}
            awards = res.get("reserve_awards") or detail.get("reserve_awards") or {}

            if rid == BESS_RESOURCE_ID:
                charge = float(detail.get("charge_mw", 0.0) or 0.0)
                discharge = float(detail.get("discharge_mw", 0.0) or 0.0)
                soc = float(detail.get("soc_mwh", 0.0) or 0.0)
                entry["bess"] = {
                    "charge_mw": charge,
                    "discharge_mw": discharge,
                    "soc_mwh": soc,
                }
                totals["total_charge_mwh"] += charge * dt
                totals["total_discharge_mwh"] += discharge * dt
                totals["throughput_mwh"] += (charge + discharge) * dt
                totals["bess_degradation_cost_dollars"] += (
                    problem.site.bess.degradation_cost_per_mwh * (charge + discharge) * dt
                )
            elif rid == SOLAR_RESOURCE_ID:
                entry["renewables"]["solar_mw"] = mw
                totals["renewables_mwh"] += mw * dt
                if problem.site.solar:
                    totals["rec_revenue_dollars"] += (
                        mw * problem.site.solar.rec_value_per_mwh * dt
                    )
            elif rid == WIND_RESOURCE_ID:
                entry["renewables"]["wind_mw"] = mw
                totals["renewables_mwh"] += mw * dt
                if problem.site.wind:
                    totals["rec_revenue_dollars"] += (
                        mw * problem.site.wind.rec_value_per_mwh * dt
                    )
            elif rid == GRID_IMPORT_RESOURCE_ID:
                entry["grid_import_mw"] = mw
                grid_import_by_period[t] = mw
                totals["grid_import_mwh"] += mw * dt
                totals["energy_import_cost_dollars"] += mw * lmp_t * dt
            elif rid == GRID_EXPORT_RESOURCE_ID:
                # Dispatchable load served = exported MW.
                served = float(detail.get("served_p_mw", abs(mw)) or 0.0)
                entry["grid_export_mw"] = served
                totals["grid_export_mwh"] += served * dt
                totals["energy_export_revenue_dollars"] += served * lmp_t * dt
            elif rid in thermal_ids:
                slot = thermal_ids[rid]
                thermal = next(
                    th for _slot, th in problem.thermal_specs() if th and th.resource_id == rid
                )
                hr_mmbtu = thermal.heat_rate_btu_per_kwh / 1000.0
                # Use per-period fuel price when set (gas-fed assets
                # share a per-period gas-price array); fall back to the
                # scalar otherwise.
                if thermal.fuel_price_per_period_per_mmbtu is not None:
                    fuel_price_t = float(
                        thermal.fuel_price_per_period_per_mmbtu[t]
                    )
                else:
                    fuel_price_t = float(thermal.fuel_price_per_mmbtu)
                fuel_cost_t = mw * hr_mmbtu * fuel_price_t * dt
                vom_t = mw * thermal.vom_per_mwh * dt
                entry["thermals"][slot] = {
                    "resource_id": rid,
                    "mw": mw,
                    "fuel_cost_dollars": fuel_cost_t,
                    "vom_dollars": vom_t,
                    "co2_tonnes": mw * thermal.co2_tonnes_per_mwh * dt,
                }
                totals["fuel_cost_dollars"] += fuel_cost_t
                totals["vom_cost_dollars"] += vom_t
                totals["co2_tonnes"] += mw * thermal.co2_tonnes_per_mwh * dt
                totals["thermal_mwh"] += mw * dt
            elif nuclear_id is not None and rid == nuclear_id:
                entry["nuclear_mw"] = mw
                totals["nuclear_mwh"] += mw * dt
            elif rid in tier_resource_ids:
                tier = tier_resource_ids[rid]
                served_mw = float(detail.get("served_p_mw", abs(mw)) or 0.0)
                cap_mw = (
                    tier.capacity_per_period_mw[t]
                    if tier.capacity_per_period_mw is not None
                    else tier.capacity_mw
                )
                curtailed_mw = max(float(cap_mw) - served_mw, 0.0)
                entry["tiers"].append(
                    {
                        "tier_id": tier.tier_id,
                        "served_mw": served_mw,
                        "curtailed_mw": curtailed_mw,
                        "voll_per_mwh": tier.voll_per_mwh,
                    }
                )
                totals["compute_revenue_dollars"] += served_mw * tier.voll_per_mwh * dt
                totals["compute_curtailment_loss_dollars"] += (
                    curtailed_mw * tier.voll_per_mwh * dt
                )
                totals["tier_served_mwh"] += served_mw * dt
                totals["tier_curtailed_mwh"] += curtailed_mw * dt

            # Accumulate AS revenue from any resource awards.
            if isinstance(awards, dict):
                for product_id, award_mw in awards.items():
                    if award_mw is None or product_id is None:
                        continue
                    award_mw = float(award_mw)
                    if abs(award_mw) < 1e-9:
                        continue
                    price = float(
                        as_price_by_product.get(product_id, [0.0] * n)[t]
                    )
                    revenue = award_mw * price * dt
                    totals["as_revenue_dollars"] += revenue
                    bucket = entry["as_awards"].setdefault(product_id, [])
                    bucket.append(
                        {
                            "resource_id": rid,
                            "award_mw": award_mw,
                            "price_per_mwh": price,
                            "revenue_dollars": revenue,
                        }
                    )

        schedule.append(entry)

    # 4-CP transmission demand charge — applied to peak grid import
    # over the flagged periods. The LP already captured this in its
    # objective; we recompute here for the report so the user sees
    # the dollar contribution explicitly.
    four_cp = problem.site.four_cp
    peak_import_mw = 0.0
    if four_cp is not None and four_cp.period_indices:
        peak_import_mw = max(
            (grid_import_by_period[p] for p in four_cp.period_indices),
            default=0.0,
        )
        totals["tx_demand_charge_dollars"] = peak_import_mw * float(four_cp.charge_per_mw)

    totals["net_pnl_dollars"] = (
        totals["compute_revenue_dollars"]
        + totals["energy_export_revenue_dollars"]
        + totals["as_revenue_dollars"]
        + totals["rec_revenue_dollars"]
        - totals["energy_import_cost_dollars"]
        - totals["fuel_cost_dollars"]
        - totals["vom_cost_dollars"]
        - totals["no_load_cost_dollars"]
        - totals["startup_cost_dollars"]
        - totals["bess_degradation_cost_dollars"]
        - totals["tx_demand_charge_dollars"]
    )
    totals["peak_grid_import_mw"] = peak_import_mw

    return {
        "periods": n,
        "period_durations_hours": list(durations),
        "schedule": schedule,
        "totals": totals,
    }


__all__ = ["extract_pl_report", "extract_pl_report_from_sequence"]
