# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Backend logic for the datacenter dashboard.

* Builds a default scenario for a 1 GW Texas-shaped DC so the page
  has something interesting to show on first load.
* Translates the client's scenario JSON into a
  :class:`DataCenterProblem` + :class:`DataCenterPolicy`, runs the
  SCUC via :func:`markets.datacenter.solve`, and flattens the result
  into a JSON-safe response the frontend renders directly.

Stateless — every solve takes a fresh scenario from the client and
runs in a throwaway workdir.
"""

from __future__ import annotations

import json
import math
import tempfile
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from markets.datacenter import (
    AsProduct,
    BessSpec,
    COMMITMENT_MODES,
    CurtailableLoadTier,
    DataCenterPolicy,
    DataCenterProblem,
    FourCpSpec,
    ItLoadSpec,
    NuclearSpec,
    PERIOD_COUPLINGS,
    SiteSpec,
    SolarSpec,
    ThermalSpec,
    WindSpec,
    solve,
)
from surge.market import (
    ECRS,
    NON_SPINNING,
    PRODUCT_BY_ID,
    REG_DOWN,
    REG_UP,
    SPINNING,
)


__all__ = [
    "COMMITMENT_MODES",
    "PERIOD_COUPLINGS",
    "available_as_products",
    "default_scenario",
    "run_solve",
]


# ---------------------------------------------------------------------------
# Default-scenario shapes (synthetic ERCOT-style profiles)
# ---------------------------------------------------------------------------


def default_lmp_shape(hours: int = 24) -> list[float]:
    """ERCOT-shaped duck curve with a strong evening peak.

    * Overnight ~$25, solar trough ~$10, evening peak ~$95, second
      shoulder $40. Matches the shape of summer-peak Houston-Hub
      day-ahead with a moderate scarcity spike.
    """
    out = []
    for h in range(hours):
        t = h % 24
        overnight = 28.0
        solar_dip = -22.0 * math.exp(-((t - 12.5) ** 2) / 9.0)
        evening_peak = 75.0 * math.exp(-((t - 18.5) ** 2) / 4.0)
        morning_ramp = 12.0 * math.exp(-((t - 7.0) ** 2) / 3.0)
        out.append(round(max(5.0, overnight + solar_dip + morning_ramp + evening_peak), 2))
    return out


def default_solar_cf(hours: int = 24) -> list[float]:
    out = []
    for h in range(hours):
        t = h % 24
        # Daylight only between 6 and 19, peak around 13.
        if t < 6 or t > 19:
            out.append(0.0)
            continue
        out.append(round(max(0.0, math.cos(math.pi * (t - 13.0) / 14.0)) ** 1.4, 3))
    return out


def default_wind_cf(hours: int = 24) -> list[float]:
    """West-Texas-style wind: night-heavy, daylight low."""
    out = []
    for h in range(hours):
        t = h % 24
        base = 0.45
        diurnal = -0.18 * math.cos((t - 3.0) * math.pi / 12.0)
        out.append(round(max(0.05, min(0.95, base + diurnal)), 3))
    return out


def default_must_serve_load(hours: int = 24, base_mw: float = 700.0) -> list[float]:
    """Datacenter baseload — flat with a small evening lift."""
    return [round(base_mw + 5.0 * math.sin((h - 18.0) * math.pi / 12.0), 2) for h in range(hours)]


def default_reg_up(hours: int = 24) -> list[float]:
    out = []
    for h in range(hours):
        t = h % 24
        base = 6.0
        morning = 8.0 * math.exp(-((t - 7.0) ** 2) / 4.0)
        evening = 14.0 * math.exp(-((t - 18.0) ** 2) / 4.0)
        out.append(round(base + morning + evening, 2))
    return out


def default_reg_down(hours: int = 24) -> list[float]:
    out = []
    for h in range(hours):
        t = h % 24
        base = 4.0
        solar = 10.0 * math.exp(-((t - 12.0) ** 2) / 6.0)
        out.append(round(base + solar, 2))
    return out


def default_ecrs(hours: int = 24) -> list[float]:
    # h%24 so the evening-peak shape tiles cleanly across multi-day
    # horizons — without the modulo the peak only fires on day 1.
    return [
        round(5.0 + 4.0 * math.exp(-(((h % 24) - 18.0) ** 2) / 6.0), 2)
        for h in range(hours)
    ]


def default_non_spin(hours: int = 24) -> list[float]:
    return [round(2.5 + 1.5 * math.sin((h - 17.0) * math.pi / 12.0), 2) for h in range(hours)]


def default_gas_price(hours: int = 24) -> list[float]:
    """Natural-gas price ($/MMBtu) feeding gas CT + fuel cell.

    Default flat $4/MMBtu — typical Henry-Hub-like baseline. The
    operator can shape this on the forecasts tab when modeling a gas-
    price spike scenario.
    """
    return [4.0] * hours


# ---------------------------------------------------------------------------
# Default scenario — a 1 GW Texas-style datacenter
# ---------------------------------------------------------------------------


def default_scenario() -> dict[str, Any]:
    # Three-day horizon by default — gives the operator a multi-day
    # view of dispatch + commitment cycling out of the box. Every
    # helper below uses an h%24 modulo so the synthetic profiles tile
    # cleanly into three identical daily patterns.
    hours = 72
    return {
        "time_axis": {
            "start_iso": datetime.now()
            .replace(hour=0, minute=0, second=0, microsecond=0)
            .isoformat(),
            "horizon_minutes": hours * 60,
            "resolution_minutes": 60,
            "periods": hours,
        },
        "site": {
            "poi_limit_mw": 1000.0,
            "it_load": {
                "must_serve_mw": default_must_serve_load(hours, base_mw=700.0),
                "tiers": [
                    {
                        "tier_id": "training",
                        "capacity_mw": 200.0,
                        "voll_per_mwh": 60.0,
                    },
                    {
                        "tier_id": "research",
                        "capacity_mw": 100.0,
                        "voll_per_mwh": 5.0,
                    },
                ],
            },
            "bess": {
                "power_charge_mw": 200.0,
                "power_discharge_mw": 200.0,
                "energy_mwh": 800.0,
                "charge_efficiency": 0.92,
                "discharge_efficiency": 0.96,
                "soc_min_fraction": 0.10,
                "soc_max_fraction": 0.95,
                "initial_soc_mwh": 400.0,
                "degradation_cost_per_mwh": 2.0,
                "daily_cycle_limit": 2.0,
                "qualified_as_products": [
                    REG_UP.id,
                    REG_DOWN.id,
                    SPINNING.id,
                    ECRS.id,
                    NON_SPINNING.id,
                ],
            },
            "solar": {
                "nameplate_mw": 250.0,
                "capacity_factors": default_solar_cf(hours),
                # REC value ($/MWh) acts as a negative marginal cost in
                # the SCUC — each MWh dispatched credits the operator
                # by this amount; each MWh curtailed forgoes it.
                "rec_value_per_mwh": 25.0,
                "qualified_as_products": [],
            },
            "wind": {
                "nameplate_mw": 150.0,
                "capacity_factors": default_wind_cf(hours),
                "rec_value_per_mwh": 30.0,
                "qualified_as_products": [],
            },
            "fuel_cell": {
                "enabled": True,
                "resource_id": "fuel_cell",
                "nameplate_mw": 200.0,
                "pmin_mw": 20.0,
                "fuel_price_per_mmbtu": 8.0,
                "heat_rate_btu_per_kwh": 6500.0,
                "vom_per_mwh": 1.5,
                "no_load_cost_per_hr": 100.0,
                "min_up_h": 2.0,
                "min_down_h": 1.0,
                "co2_tonnes_per_mwh": 0.30,
                "startup_cost_tiers": [
                    {"max_offline_hours": 4.0, "cost": 500.0},
                ],
                "ramp_up_mw_per_min": 5.0,
                "ramp_down_mw_per_min": 5.0,
                "qualified_as_products": [REG_UP.id, REG_DOWN.id, SPINNING.id, ECRS.id],
            },
            "gas_ct": {
                "enabled": True,
                "resource_id": "gas_ct",
                "nameplate_mw": 400.0,
                "pmin_mw": 50.0,
                "fuel_price_per_mmbtu": 4.0,
                "heat_rate_btu_per_kwh": 9500.0,
                "vom_per_mwh": 4.0,
                "no_load_cost_per_hr": 500.0,
                "min_up_h": 4.0,
                "min_down_h": 4.0,
                "co2_tonnes_per_mwh": 0.45,
                "startup_cost_tiers": [
                    {"max_offline_hours": 8.0, "cost": 4000.0},
                    {"max_offline_hours": 24.0, "cost": 8000.0},
                ],
                "ramp_up_mw_per_min": 10.0,
                "ramp_down_mw_per_min": 10.0,
                "qualified_as_products": [REG_UP.id, SPINNING.id, ECRS.id],
            },
            "diesel": {
                "enabled": True,
                "resource_id": "diesel",
                "nameplate_mw": 50.0,
                "pmin_mw": 0.0,
                "fuel_price_per_mmbtu": 20.0,
                "heat_rate_btu_per_kwh": 10500.0,
                "vom_per_mwh": 8.0,
                "no_load_cost_per_hr": 0.0,
                "min_up_h": 0.0,
                "min_down_h": 0.0,
                "co2_tonnes_per_mwh": 0.74,
                "startup_cost_tiers": [],
                "ramp_up_mw_per_min": 25.0,
                "ramp_down_mw_per_min": 25.0,
                "qualified_as_products": [NON_SPINNING.id],
            },
            "nuclear": {
                "enabled": False,
                "resource_id": "nuclear",
                "nameplate_mw": 100.0,
                "marginal_cost_per_mwh": 10.0,
                "availability_per_period": [1.0] * hours,
                "qualified_as_products": [],
            },
        },
        "lmp_forecast_per_mwh": default_lmp_shape(hours),
        # Per-period natural-gas price ($/MMBtu) shared by every gas-fed
        # thermal — currently the gas CT + fuel cell. Diesel keeps its
        # own scalar fuel price.
        "natural_gas_price_per_mmbtu": default_gas_price(hours),
        "as_products": [
            {
                "product_id": REG_UP.id,
                "title": REG_UP.name,
                "direction": REG_UP.direction,
                "price_forecast_per_mwh": default_reg_up(hours),
            },
            {
                "product_id": REG_DOWN.id,
                "title": REG_DOWN.name,
                "direction": REG_DOWN.direction,
                "price_forecast_per_mwh": default_reg_down(hours),
            },
            {
                "product_id": ECRS.id,
                "title": ECRS.name,
                "direction": ECRS.direction,
                "price_forecast_per_mwh": default_ecrs(hours),
            },
            {
                "product_id": NON_SPINNING.id,
                "title": NON_SPINNING.name,
                "direction": NON_SPINNING.direction,
                "price_forecast_per_mwh": default_non_spin(hours),
            },
        ],
        "four_cp": {
            "enabled": False,
            "annual_charge_per_mw_year": 40_000.0,
            "expected_intervals_per_year": 4,
            # Per-period boolean flag; the user clicks to mark periods
            # they expect to fall on a 4-CP interval.
            "period_flags": [False] * hours,
        },
        "policy": {
            "commitment_mode": "optimize",
            "period_coupling": "coupled",
            "lp_solver": "highs",
            "mip_rel_gap": 1e-3,
            "mip_time_limit_secs": 120.0,
            "enforce_reserve_capacity": False,
        },
    }


def available_as_products() -> list[dict[str, Any]]:
    """Menu of AS products the user can add to the scenario."""
    return [
        {
            "product_id": p.id,
            "title": p.name,
            "direction": p.direction,
            "qualification": p.qualification,
        }
        for p in (REG_UP, REG_DOWN, SPINNING, ECRS, NON_SPINNING)
    ]


# ---------------------------------------------------------------------------
# Solve
# ---------------------------------------------------------------------------


def run_solve(scenario: dict[str, Any]) -> dict[str, Any]:
    problem, policy = _build_problem_and_policy(scenario)
    with tempfile.TemporaryDirectory(prefix="surge-datacenter-") as tmp:
        report = solve(problem, Path(tmp), policy=policy, label="dashboard")
        workdir = Path(tmp)
        pl_path = workdir / "pnl-report.json"
        if not pl_path.is_file():
            return {
                "status": report["status"],
                "error": report.get("error"),
                "elapsed_secs": report.get("elapsed_secs"),
                "pl_summary": None,
                "schedule": [],
            }
        pl = json.loads(pl_path.read_text(encoding="utf-8"))

    time_axis = scenario["time_axis"]
    start = datetime.fromisoformat(time_axis["start_iso"])
    resolution_minutes = int(time_axis["resolution_minutes"])
    period_times_iso = [
        (start + timedelta(minutes=resolution_minutes * t)).isoformat()
        for t in range(problem.periods)
    ]

    return {
        "status": report["status"],
        "error": report.get("error"),
        "elapsed_secs": report.get("elapsed_secs"),
        "periods": problem.periods,
        "period_times_iso": period_times_iso,
        "pl_summary": pl["totals"],
        "schedule": pl["schedule"],
        "policy_echo": report.get("policy", {}),
    }


def _build_problem_and_policy(
    scenario: dict[str, Any],
) -> tuple[DataCenterProblem, DataCenterPolicy]:
    time_axis = scenario.get("time_axis") or {}
    periods = int(time_axis.get("periods") or 24)
    resolution_minutes = int(time_axis.get("resolution_minutes") or 60)
    period_hours = [resolution_minutes / 60.0] * periods

    site_dict = scenario.get("site") or {}
    bess_dict = site_dict.get("bess") or {}
    bess = BessSpec(
        power_charge_mw=float(bess_dict.get("power_charge_mw", 0.0)),
        power_discharge_mw=float(bess_dict.get("power_discharge_mw", 0.0)),
        energy_mwh=float(bess_dict.get("energy_mwh", 0.0)),
        charge_efficiency=float(bess_dict.get("charge_efficiency", 0.92)),
        discharge_efficiency=float(bess_dict.get("discharge_efficiency", 0.96)),
        soc_min_fraction=float(bess_dict.get("soc_min_fraction", 0.10)),
        soc_max_fraction=float(bess_dict.get("soc_max_fraction", 0.95)),
        initial_soc_mwh=_optional_float(bess_dict.get("initial_soc_mwh")),
        degradation_cost_per_mwh=float(bess_dict.get("degradation_cost_per_mwh", 2.0)),
        daily_cycle_limit=_optional_float(bess_dict.get("daily_cycle_limit")),
        qualified_as_products=tuple(bess_dict.get("qualified_as_products") or ()),
    )

    it_dict = site_dict.get("it_load") or {}
    must_serve = list(it_dict.get("must_serve_mw") or [])
    tiers = [
        CurtailableLoadTier(
            tier_id=str(t["tier_id"]),
            capacity_mw=float(t["capacity_mw"]),
            voll_per_mwh=float(t["voll_per_mwh"]),
            capacity_per_period_mw=t.get("capacity_per_period_mw"),
        )
        for t in (it_dict.get("tiers") or [])
    ]
    it_load = ItLoadSpec(must_serve_mw=must_serve, tiers=tiers)

    solar = _renewable_from_dict(site_dict.get("solar"), SolarSpec)
    wind = _renewable_from_dict(site_dict.get("wind"), WindSpec)

    # Per-period natural-gas price feeding gas-fed thermals. When set,
    # each gas-fed thermal's `fuel_price_per_period_per_mmbtu` is
    # populated so the LP's marginal cost varies hour-by-hour.
    gas_price_per_period = scenario.get("natural_gas_price_per_mmbtu")
    if not isinstance(gas_price_per_period, list) or len(gas_price_per_period) != periods:
        gas_price_per_period = None
    else:
        gas_price_per_period = [float(v) for v in gas_price_per_period]
    fuel_cell = _thermal_from_dict(site_dict.get("fuel_cell"), gas_price_per_period)
    gas_ct = _thermal_from_dict(site_dict.get("gas_ct"), gas_price_per_period)
    # Diesel runs on diesel, not natural gas — keep its own scalar price.
    diesel = _thermal_from_dict(site_dict.get("diesel"))
    nuclear = _nuclear_from_dict(site_dict.get("nuclear"))

    four_cp = _four_cp_from_scenario(scenario.get("four_cp"))

    site = SiteSpec(
        poi_limit_mw=float(site_dict.get("poi_limit_mw", 0.0)),
        it_load=it_load,
        bess=bess,
        solar=solar,
        wind=wind,
        fuel_cell=fuel_cell,
        gas_ct=gas_ct,
        diesel=diesel,
        nuclear=nuclear,
        four_cp=four_cp,
    )

    as_products = []
    for entry in scenario.get("as_products") or []:
        pid = str(entry.get("product_id"))
        product = PRODUCT_BY_ID.get(pid)
        if product is None:
            continue
        as_products.append(
            AsProduct(
                product_def=product,
                price_forecast_per_mwh=[
                    float(v) for v in entry.get("price_forecast_per_mwh") or []
                ],
            )
        )

    problem = DataCenterProblem(
        period_durations_hours=period_hours,
        lmp_forecast_per_mwh=[float(v) for v in scenario.get("lmp_forecast_per_mwh") or []],
        site=site,
        as_products=as_products,
    )

    pol = scenario.get("policy") or {}
    policy = DataCenterPolicy(
        commitment_mode=str(pol.get("commitment_mode") or "optimize"),
        period_coupling=str(pol.get("period_coupling") or "coupled"),
        lp_solver=str(pol.get("lp_solver") or "highs"),
        mip_rel_gap=float(pol.get("mip_rel_gap") or 1e-3),
        mip_time_limit_secs=float(pol.get("mip_time_limit_secs") or 120.0),
        enforce_reserve_capacity=bool(pol.get("enforce_reserve_capacity", False)),
    )

    return problem, policy


def _renewable_from_dict(d: dict[str, Any] | None, cls: Any) -> Any:
    if not d:
        return None
    return cls(
        nameplate_mw=float(d.get("nameplate_mw") or 0.0),
        capacity_factors=[float(v) for v in d.get("capacity_factors") or []],
        rec_value_per_mwh=float(d.get("rec_value_per_mwh") or 0.0),
        qualified_as_products=tuple(d.get("qualified_as_products") or ()),
    )


def _thermal_from_dict(
    d: dict[str, Any] | None,
    fuel_price_per_period: list[float] | None = None,
) -> ThermalSpec | None:
    if not d or not d.get("enabled", True):
        return None
    return ThermalSpec(
        resource_id=str(d.get("resource_id")),
        nameplate_mw=float(d.get("nameplate_mw") or 0.0),
        pmin_mw=float(d.get("pmin_mw") or 0.0),
        fuel_price_per_mmbtu=float(d.get("fuel_price_per_mmbtu") or 0.0),
        fuel_price_per_period_per_mmbtu=fuel_price_per_period,
        heat_rate_btu_per_kwh=float(d.get("heat_rate_btu_per_kwh") or 0.0),
        vom_per_mwh=float(d.get("vom_per_mwh") or 0.0),
        no_load_cost_per_hr=float(d.get("no_load_cost_per_hr") or 0.0),
        startup_cost_tiers=list(d.get("startup_cost_tiers") or []),
        min_up_h=float(d.get("min_up_h") or 0.0),
        min_down_h=float(d.get("min_down_h") or 0.0),
        ramp_up_mw_per_min=_optional_float(d.get("ramp_up_mw_per_min")),
        ramp_down_mw_per_min=_optional_float(d.get("ramp_down_mw_per_min")),
        co2_tonnes_per_mwh=float(d.get("co2_tonnes_per_mwh") or 0.0),
        qualified_as_products=tuple(d.get("qualified_as_products") or ()),
    )


def _nuclear_from_dict(d: dict[str, Any] | None) -> NuclearSpec | None:
    if not d or not d.get("enabled", False):
        return None
    return NuclearSpec(
        resource_id=str(d.get("resource_id") or "nuclear"),
        nameplate_mw=float(d.get("nameplate_mw") or 0.0),
        marginal_cost_per_mwh=float(d.get("marginal_cost_per_mwh") or 0.0),
        availability_per_period=d.get("availability_per_period"),
        qualified_as_products=tuple(d.get("qualified_as_products") or ()),
    )


def _four_cp_from_scenario(d: dict[str, Any] | None) -> FourCpSpec | None:
    if not d or not d.get("enabled", False):
        return None
    period_flags = d.get("period_flags") or []
    annual = float(d.get("annual_charge_per_mw_year") or 0.0)
    expected = max(int(d.get("expected_intervals_per_year") or 4), 1)
    flagged = [i for i, flag in enumerate(period_flags) if flag]
    if not flagged or annual <= 0.0:
        return None
    # When the user marks N periods within the simulated horizon as a
    # 4-CP interval, the expected $/MW seen by the LP equals the
    # annualised rate divided by the expected number of intervals
    # observed across the year. This treats each flagged period as a
    # candidate for one of the four annual peaks.
    return FourCpSpec(
        period_indices=flagged,
        charge_per_mw=annual / float(expected),
    )


def _optional_float(v: Any) -> float | None:
    if v is None:
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None
