# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Backend logic for the battery dashboard.

Two jobs:

* Produce a good **default scenario** so the page has something
  interesting to show on first load — duck-curve LMP, representative
  AS prices, a 25 MW / 100 MWh BESS spec.
* Convert the client's scenario JSON into a :class:`BatteryProblem`
  + :class:`BatteryPolicy`, run :func:`markets.battery.solve`, and
  flatten the results into a JSON-safe response the frontend
  renders directly.

The dashboard is stateless — every solve takes a fresh scenario
from the client and runs in a throwaway workdir.
"""

from __future__ import annotations

import json
import math
import tempfile
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from markets.battery import (
    AsProduct,
    BatteryPolicy,
    BatteryProblem,
    PwlBidStrategy,
    SiteSpec,
    solve,
)
from surge.market import (
    NON_SPINNING,
    PRODUCT_BY_ID,
    REG_DOWN,
    REG_UP,
    SPINNING,
)


# ---------------------------------------------------------------------------
# Defaults — duck curve + AS shapes
# ---------------------------------------------------------------------------


def default_lmp_shape(hours: int = 24) -> list[float]:
    """Classic duck curve. Values roughly in $20–$90 range.

    * Overnight shoulder ~$30
    * Solar trough at noon ~$15
    * Evening peak 18:00 ~$90
    * Second shoulder 22:00 ~$45
    """
    base = []
    for h in range(hours):
        # Mix sines: overnight baseline, solar dip, evening ramp.
        t = h % 24
        overnight = 32.0
        solar_trough = -20.0 * math.exp(-((t - 12.0) ** 2) / 8.0)
        evening_peak = 60.0 * math.exp(-((t - 18.5) ** 2) / 3.5)
        morning_ramp = 15.0 * math.exp(-((t - 7.5) ** 2) / 3.0)
        value = max(5.0, overnight + solar_trough + morning_ramp + evening_peak)
        base.append(round(value, 2))
    return base


def default_reg_up_shape(hours: int = 24) -> list[float]:
    """Reg-up prices — spike during ramps."""
    out = []
    for h in range(hours):
        t = h % 24
        baseline = 5.0
        morning_ramp = 8.0 * math.exp(-((t - 7.0) ** 2) / 4.0)
        evening_ramp = 14.0 * math.exp(-((t - 18.0) ** 2) / 4.0)
        out.append(round(baseline + morning_ramp + evening_ramp, 2))
    return out


def default_reg_down_shape(hours: int = 24) -> list[float]:
    """Reg-down prices — spike during solar over-supply."""
    out = []
    for h in range(hours):
        t = h % 24
        baseline = 4.0
        solar_ramp = 10.0 * math.exp(-((t - 12.0) ** 2) / 6.0)
        out.append(round(baseline + solar_ramp, 2))
    return out


def default_spin_shape(hours: int = 24) -> list[float]:
    """Spinning reserve — relatively flat."""
    return [round(3.0 + 1.5 * math.sin((h - 17.0) * math.pi / 12.0), 2) for h in range(hours)]


def default_scenario() -> dict[str, Any]:
    """The seed scenario every fresh page load sees."""
    hours = 24
    return {
        "time_axis": {
            "start_iso": datetime.now().replace(
                hour=0, minute=0, second=0, microsecond=0
            ).isoformat(),
            "horizon_minutes": hours * 60,
            "resolution_minutes": 60,
            "periods": hours,
        },
        "site": {
            "bess_power_charge_mw": 25.0,
            "bess_power_discharge_mw": 25.0,
            "bess_energy_mwh": 100.0,
            "bess_charge_efficiency": 0.90,
            "bess_discharge_efficiency": 0.98,
            "bess_soc_min_fraction": 0.10,
            "bess_soc_max_fraction": 0.95,
            # 5%-wide foldback ramps at each end: discharge cap derates
            # from 0.15 down to soc_min=0.10; charge cap derates from
            # 0.90 up to soc_max=0.95.
            "bess_discharge_foldback_fraction": 0.15,
            "bess_charge_foldback_fraction": 0.90,
            "bess_initial_soc_mwh": 50.0,
            "bess_degradation_cost_per_mwh": 2.0,
        },
        "lmp_forecast_per_mwh": default_lmp_shape(hours),
        "as_products": [
            {
                "product_id": REG_UP.id,
                "title": REG_UP.name,
                "direction": REG_UP.direction,
                "price_forecast_per_mwh": default_reg_up_shape(hours),
            },
            {
                "product_id": REG_DOWN.id,
                "title": REG_DOWN.name,
                "direction": REG_DOWN.direction,
                "price_forecast_per_mwh": default_reg_down_shape(hours),
            },
            {
                "product_id": SPINNING.id,
                "title": SPINNING.name,
                "direction": SPINNING.direction,
                "price_forecast_per_mwh": default_spin_shape(hours),
            },
        ],
        "policy": {
            "dispatch_mode": "optimal_foresight",
            "period_coupling": "coupled",
        },
        "pwl_strategy": default_pwl_strategy(),
    }


def default_pwl_strategy() -> dict[str, Any]:
    """A 4-segment starter offer curve — staircase bids in both directions.

    ``discharge_offer_segments`` and ``charge_bid_segments`` are cumulative
    (MW, $/MWh) pairs. The first segment is the price at which the battery
    starts to engage; the last is where it reaches full capacity.
    """
    return {
        "discharge_offer_segments": [
            [6.0, 40.0],
            [12.0, 55.0],
            [18.0, 75.0],
            [25.0, 95.0],
        ],
        "charge_bid_segments": [
            [6.0, 35.0],
            [12.0, 25.0],
            [18.0, 15.0],
            [25.0, 5.0],
        ],
        "as_offer_prices_per_mwh": {
            REG_UP.id: 2.0,
            REG_DOWN.id: 2.0,
            SPINNING.id: 1.0,
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
        for p in (REG_UP, REG_DOWN, SPINNING, NON_SPINNING)
    ]


# ---------------------------------------------------------------------------
# Solve
# ---------------------------------------------------------------------------


def run_solve(scenario: dict[str, Any]) -> dict[str, Any]:
    """Run the battery market solve on a scenario dict and flatten results."""
    problem, policy = _build_problem_and_policy(scenario)
    with tempfile.TemporaryDirectory(prefix="surge-battery-") as tmp:
        report = solve(problem, Path(tmp), policy=policy, label="dashboard")
        workdir = Path(tmp)
        revenue_path = workdir / "revenue-report.json"
        if not revenue_path.is_file():
            return {
                "status": report["status"],
                "error": report.get("error"),
                "elapsed_secs": report.get("elapsed_secs"),
                "revenue_summary": None,
                "schedule": [],
                "as_breakdown": [],
            }
        revenue = json.loads(revenue_path.read_text(encoding="utf-8"))

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
        "revenue_summary": revenue["totals"],
        "schedule": revenue["schedule"],
        "as_breakdown": revenue["as_breakdown"],
        "policy_echo": report.get("policy", {}),
    }


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _coerce_per_period_prices(
    raw: Any, periods: int, num_segments: int
) -> list[list[float | None] | None] | None:
    """Normalize a per-period PWL price override matrix.

    Expects a length-``periods`` list where each entry is either
    ``None`` (use baseline for that period) or a length-``num_segments``
    list whose own entries are ``None`` (use baseline for that segment)
    or a price float. Returns ``None`` if no actual overrides are set.
    """
    if not raw or not isinstance(raw, list) or len(raw) != periods:
        return None
    out: list[list[float | None] | None] = []
    any_custom = False
    for entry in raw:
        if entry is None:
            out.append(None)
            continue
        if not isinstance(entry, list) or len(entry) != num_segments:
            out.append(None)
            continue
        row: list[float | None] = []
        for v in entry:
            if v is None:
                row.append(None)
            else:
                row.append(float(v))
                any_custom = True
        out.append(row)
    return out if any_custom else None


def _coerce_segments(raw: Any) -> list[tuple[float, float]]:
    """Normalize a JSON segment list into cumulative (MW, $/MWh) tuples.

    Accepts lists of [mw, price] pairs or dicts with ``mw``/``price`` keys.
    Zero-MW entries are dropped; the result is sorted ascending by MW.
    """
    if not raw:
        return []
    out: list[tuple[float, float]] = []
    for entry in raw:
        if isinstance(entry, dict):
            mw = float(entry.get("mw", 0.0))
            price = float(entry.get("price", 0.0))
        else:
            mw, price = float(entry[0]), float(entry[1])
        if mw > 1e-9:
            out.append((mw, price))
    out.sort(key=lambda p: p[0])
    return out


def _build_problem_and_policy(
    scenario: dict[str, Any],
) -> tuple[BatteryProblem, BatteryPolicy]:
    time_axis = scenario["time_axis"]
    resolution_minutes = int(time_axis["resolution_minutes"])
    horizon_minutes = int(time_axis["horizon_minutes"])
    periods = int(time_axis.get("periods") or horizon_minutes // resolution_minutes)
    duration_hours = resolution_minutes / 60.0
    period_durations_hours = [duration_hours] * periods

    lmp = list(scenario["lmp_forecast_per_mwh"])[:periods]
    if len(lmp) != periods:
        raise ValueError(
            f"lmp_forecast_per_mwh has {len(lmp)} values, expected {periods}"
        )

    as_products: list[AsProduct] = []
    for entry in scenario.get("as_products", []):
        product_id = str(entry["product_id"])
        product_def = PRODUCT_BY_ID.get(product_id)
        if product_def is None:
            raise ValueError(f"unknown AS product: {product_id!r}")
        prices = list(entry["price_forecast_per_mwh"])[:periods]
        if len(prices) != periods:
            raise ValueError(
                f"AS product {product_id}: price forecast has {len(prices)} values, "
                f"expected {periods}"
            )
        as_products.append(AsProduct(product_def, prices))

    site_raw = scenario["site"]
    # POI is no longer configured from the UI — BESS pmin/pmax already
    # bound the injection. Fall back to the discharge MW (plus a small
    # cushion) so the interconnection never binds on its own.
    charge_mw = float(site_raw["bess_power_charge_mw"])
    discharge_mw = float(site_raw["bess_power_discharge_mw"])
    poi_limit = float(site_raw.get("poi_limit_mw") or max(charge_mw, discharge_mw) + 1.0)
    # Back-compat: legacy scenarios carry ``bess_round_trip_efficiency``;
    # split it sqrt-wise if the explicit charge/discharge fields are absent.
    if "bess_charge_efficiency" in site_raw or "bess_discharge_efficiency" in site_raw:
        eta_ch = float(site_raw.get("bess_charge_efficiency", 0.90))
        eta_dis = float(site_raw.get("bess_discharge_efficiency", 0.98))
    elif "bess_round_trip_efficiency" in site_raw:
        leg = max(0.0, float(site_raw["bess_round_trip_efficiency"])) ** 0.5
        eta_ch = eta_dis = leg
    else:
        eta_ch, eta_dis = 0.90, 0.98
    def _optional_fraction(key: str) -> float | None:
        raw = site_raw.get(key)
        if raw is None or raw == "":
            return None
        return float(raw)

    site = SiteSpec(
        poi_limit_mw=poi_limit,
        bess_power_charge_mw=charge_mw,
        bess_power_discharge_mw=discharge_mw,
        bess_energy_mwh=float(site_raw["bess_energy_mwh"]),
        bess_charge_efficiency=eta_ch,
        bess_discharge_efficiency=eta_dis,
        bess_soc_min_fraction=float(site_raw["bess_soc_min_fraction"]),
        bess_soc_max_fraction=float(site_raw["bess_soc_max_fraction"]),
        bess_discharge_foldback_fraction=_optional_fraction("bess_discharge_foldback_fraction"),
        bess_charge_foldback_fraction=_optional_fraction("bess_charge_foldback_fraction"),
        bess_initial_soc_mwh=(
            float(site_raw["bess_initial_soc_mwh"])
            if site_raw.get("bess_initial_soc_mwh") is not None
            else None
        ),
        bess_degradation_cost_per_mwh=float(
            site_raw.get("bess_degradation_cost_per_mwh", 0.0)
        ),
    )

    policy_raw = scenario.get("policy") or {}
    dispatch_mode = str(policy_raw.get("dispatch_mode", "optimal_foresight"))
    period_coupling = str(policy_raw.get("period_coupling", "coupled"))

    pwl_strategy: PwlBidStrategy | None = None
    if dispatch_mode == "pwl_offers":
        pwl_raw = scenario.get("pwl_strategy") or default_pwl_strategy()
        as_offers = {
            str(k): float(v)
            for k, v in (pwl_raw.get("as_offer_prices_per_mwh") or {}).items()
        }
        if "discharge_offer_segments" in pwl_raw or "charge_bid_segments" in pwl_raw:
            disc = _coerce_segments(pwl_raw.get("discharge_offer_segments"))
            chrg = _coerce_segments(pwl_raw.get("charge_bid_segments"))
            if not disc:
                disc = [(site.bess_power_discharge_mw, 50.0)]
            if not chrg:
                chrg = [(site.bess_power_charge_mw, 30.0)]
            disc_per_period = _coerce_per_period_prices(
                pwl_raw.get("discharge_offer_price_per_period"), periods, len(disc)
            )
            chrg_per_period = _coerce_per_period_prices(
                pwl_raw.get("charge_bid_price_per_period"), periods, len(chrg)
            )
            pwl_strategy = PwlBidStrategy(
                discharge_offer_segments=disc,
                charge_bid_segments=chrg,
                as_offer_prices_per_mwh=as_offers,
                discharge_offer_price_per_period=disc_per_period,
                charge_bid_price_per_period=chrg_per_period,
            )
        else:
            pwl_strategy = PwlBidStrategy.flat(
                discharge_capacity_mw=float(pwl_raw.get("discharge_capacity_mw", site.bess_power_discharge_mw)),
                discharge_price=float(pwl_raw.get("discharge_price", 50.0)),
                charge_capacity_mw=float(pwl_raw.get("charge_capacity_mw", site.bess_power_charge_mw)),
                charge_price=float(pwl_raw.get("charge_price", 30.0)),
                as_offer_prices_per_mwh=as_offers,
            )

    problem = BatteryProblem(
        period_durations_hours=period_durations_hours,
        lmp_forecast_per_mwh=[float(v) for v in lmp],
        site=site,
        as_products=as_products,
        pwl_strategy=pwl_strategy,
    )
    policy = BatteryPolicy(
        dispatch_mode=dispatch_mode,
        period_coupling=period_coupling,
        lp_solver=str(policy_raw.get("lp_solver", "highs")),
    )
    return problem, policy


__all__ = [
    "available_as_products",
    "default_lmp_shape",
    "default_pwl_strategy",
    "default_scenario",
    "run_solve",
]
