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


def default_ace_distribution(hours: int) -> dict[str, Any]:
    """BA Area Control Error distribution per period (% terms).

    +100 % means deploy reg-down for the entire period; −100 % means
    deploy reg-up for the entire period. Default P50 is flat 0 with a
    symmetric ±20 % band so the user immediately sees expected gross
    deployment effects against awarded regulation; raise the spread to
    widen the band, set P50 non-zero to bias deployment one way.
    """
    return {
        "shape": "gaussian",
        "spread_fraction": 0.20,
        "p10": [-20.0] * hours,
        "p50": [0.0] * hours,
        "p90": [20.0] * hours,
        # Default ACE granularity is 5-minute sub-intervals inside an
        # hourly economic period. Affects MC variance only — expected
        # post-hoc deployment is independent of sub-period count.
        "n_sub_periods": 12,
        "editable": False,
    }


def default_cr_distribution(hours: int) -> dict[str, Any]:
    """BA contingency-reserve probability distribution per period (% terms).

    0 % means no contingency expected; 100 % means certain contingency
    that calls on the operator's spinning reserves for the whole period.
    Default P10/P50/P90 = 0/1/3 % — a small baseline contingency
    probability with the lower edge floored at 0 (probabilities can't go
    negative). With ``spread_fraction=0.02`` and ``lower_floor=0``,
    the canonical symmetric formula collapses the lower side onto 0 and
    leaves the upper edge at P50+2, so the saved arrays round-trip
    cleanly when the user touches the spread input.
    """
    return {
        "shape": "gaussian",
        "spread_fraction": 0.02,
        "p10": [0.0] * hours,
        "p50": [1.0] * hours,
        "p90": [3.0] * hours,
        "n_sub_periods": 1,
        "editable": False,
    }


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
            # Hard cap on per-day FEC. ``None`` = unlimited; the UI
            # seeds 2 cycles/day so a fresh page load already shows
            # the constraint binding under the default duck-curve.
            "bess_daily_cycle_limit": 2.0,
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
            "enforce_reserve_soc_capacity": False,
        },
        "pwl_strategy": default_pwl_strategy(),
        # BA ACE and CR distributions — drive post-clearing AS deployment
        # math. Defaults are flat zero so behaviour is unchanged until
        # the operator dials in a spread.
        "distributions": {
            "ba_ace": default_ace_distribution(hours),
            "cr_pct": default_cr_distribution(hours),
        },
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

    # Post-clearing AS-deployment overlay: expected gross deployment per
    # period given the BA ACE / CR distributions, applied to a parallel
    # SOC trajectory the dashboard renders alongside the cleared one.
    as_implied = _compute_as_implied(scenario, problem, revenue)

    return {
        "status": report["status"],
        "error": report.get("error"),
        "elapsed_secs": report.get("elapsed_secs"),
        "periods": problem.periods,
        "period_times_iso": period_times_iso,
        "revenue_summary": revenue["totals"],
        "schedule": revenue["schedule"],
        "as_breakdown": revenue["as_breakdown"],
        "as_implied": as_implied,
        "policy_echo": report.get("policy", {}),
    }


def _compute_as_implied(
    scenario: dict[str, Any],
    problem: BatteryProblem,
    revenue: dict[str, Any],
) -> dict[str, Any]:
    """Per-period expected gross AS deployment + a parallel SOC trace.

    Reads ``distributions.ba_ace`` and ``distributions.cr_pct`` from the
    scenario, computes expected gross regulation / contingency deployment
    against the cleared AS awards, and propagates that as a delta on the
    cleared SOC trajectory.

    Reg-up and synchronized/non-spin awards translate into discharge MW
    (battery delivers the reserve, lowering SOC); reg-down translates into
    charge MW (battery absorbs, raising SOC).

    Returns a dict with per-period entries plus aggregate totals. Empty
    when the user hasn't supplied ACE/CR distributions.
    """
    dists = scenario.get("distributions") or {}
    ace = dists.get("ba_ace")
    cr = dists.get("cr_pct")
    schedule = revenue.get("schedule") or []
    as_breakdown = revenue.get("as_breakdown") or []
    if not schedule:
        return _empty_as_implied(0)

    n = len(schedule)
    awards_by_period: list[dict[str, float]] = [{} for _ in range(n)]
    for entry in as_breakdown:
        t = int(entry.get("period", 0))
        if 0 <= t < n:
            for award in entry.get("awards") or []:
                pid = str(award.get("product_id"))
                mw = float(award.get("award_mw") or 0.0)
                if pid:
                    awards_by_period[t][pid] = mw

    e_regup = _expected_neg_rectified(ace, n)  # E[max(-ACE/100, 0)]
    e_regdown = _expected_pos_rectified(ace, n)  # E[max(+ACE/100, 0)]
    e_cr = _expected_mean_clamped(cr, n, lo=0.0, hi=100.0)  # E[CR/100]

    site = problem.site
    eta_ch = site.bess_charge_efficiency
    eta_dis = site.bess_discharge_efficiency
    energy_capacity = site.bess_energy_mwh
    soc_min = site.bess_soc_min_fraction * energy_capacity
    soc_max = site.bess_soc_max_fraction * energy_capacity

    # Reserve-product → which event drives its deployment.
    # Up-direction reg products respond to negative ACE; the down-direction
    # reg product responds to positive ACE; spinning / non-spin respond to
    # CR. Anything else falls through to the up-ACE bucket as a sane
    # default.
    UP_REG_IDS = {"reg_up"}
    DOWN_REG_IDS = {"reg_down"}
    CONTINGENCY_IDS = {"syn", "nsyn"}

    rows: list[dict[str, Any]] = []
    soc_traj_cleared: list[float] = [
        float(s.get("soc_mwh") or 0.0) for s in schedule
    ]
    soc_drift = 0.0
    soc_traj_implied: list[float] = []
    for t, sched in enumerate(schedule):
        dt_h = float(sched.get("duration_hours") or 0.0)
        awards = awards_by_period[t]
        # Per-product expected deployment so the chart can split each
        # cleared AS reservation bar into a "deployed" portion and an
        # "undeployed" portion (instead of stacking extra bars and
        # appearing to over-utilise the inverter).
        awards_implied: dict[str, float] = {}
        for pid, award_mw in awards.items():
            if pid in UP_REG_IDS:
                factor = e_regup[t]
            elif pid in DOWN_REG_IDS:
                factor = e_regdown[t]
            elif pid in CONTINGENCY_IDS:
                factor = e_cr[t]
            else:
                factor = 0.0
            awards_implied[pid] = award_mw * factor

        regup_dep_mw = sum(awards_implied[pid] for pid in UP_REG_IDS if pid in awards)
        regdown_dep_mw = sum(
            awards_implied[pid] for pid in DOWN_REG_IDS if pid in awards
        )
        spin_dep_mw = sum(
            awards_implied[pid] for pid in CONTINGENCY_IDS if pid in awards
        )

        discharge_mw = regup_dep_mw + spin_dep_mw
        charge_mw = regdown_dep_mw

        delta_soc = (charge_mw * eta_ch - discharge_mw / max(eta_dis, 1e-9)) * dt_h
        soc_drift += delta_soc
        soc_traj_implied.append(soc_traj_cleared[t] + soc_drift)

        rows.append(
            {
                "period": t,
                "regup_deploy_mw": regup_dep_mw,
                "regdown_deploy_mw": regdown_dep_mw,
                "spin_deploy_mw": spin_dep_mw,
                "implied_charge_mw": charge_mw,
                "implied_discharge_mw": discharge_mw,
                "awards_implied": awards_implied,
                "soc_mwh_implied": soc_traj_implied[t],
                "soc_mwh_cleared": soc_traj_cleared[t],
            }
        )

    # Bound violations vs. the operating SOC envelope. Reported, not
    # corrected — the LP didn't reserve headroom for expected
    # deployment, so the implied trajectory may exit [soc_min, soc_max].
    violation_count = 0
    max_above = 0.0
    max_below = 0.0
    for v in soc_traj_implied:
        if v > soc_max:
            violation_count += 1
            max_above = max(max_above, v - soc_max)
        elif v < soc_min:
            violation_count += 1
            max_below = max(max_below, soc_min - v)

    total_implied_charge_mwh = sum(
        r["implied_charge_mw"] * float(schedule[r["period"]].get("duration_hours") or 0.0)
        for r in rows
    )
    total_implied_discharge_mwh = sum(
        r["implied_discharge_mw"] * float(schedule[r["period"]].get("duration_hours") or 0.0)
        for r in rows
    )

    return {
        "rows": rows,
        "totals": {
            "implied_charge_mwh": total_implied_charge_mwh,
            "implied_discharge_mwh": total_implied_discharge_mwh,
            "implied_throughput_mwh": total_implied_charge_mwh
            + total_implied_discharge_mwh,
            "soc_violations": violation_count,
            "soc_max_above_mwh": max_above,
            "soc_max_below_mwh": max_below,
            "soc_drift_end_mwh": soc_drift,
        },
        # Per-period deployment fractions (in %) the post-hoc math
        # actually used. The dashboard overlays these on the BA ACE and
        # CR % editable charts so the user can sanity-check that the
        # distribution they drew produces the expected utilisation.
        "factors_pct": {
            "regup": [v * 100.0 for v in e_regup],
            "regdown": [v * 100.0 for v in e_regdown],
            "cr": [v * 100.0 for v in e_cr],
        },
        "soc_min_mwh": soc_min,
        "soc_max_mwh": soc_max,
    }


def _empty_as_implied(n: int) -> dict[str, Any]:
    return {
        "rows": [],
        "totals": {
            "implied_charge_mwh": 0.0,
            "implied_discharge_mwh": 0.0,
            "implied_throughput_mwh": 0.0,
            "soc_violations": 0,
            "soc_max_above_mwh": 0.0,
            "soc_max_below_mwh": 0.0,
            "soc_drift_end_mwh": 0.0,
        },
        "factors_pct": {
            "regup": [0.0] * n,
            "regdown": [0.0] * n,
            "cr": [0.0] * n,
        },
        "soc_min_mwh": 0.0,
        "soc_max_mwh": 0.0,
    }


# ---------------------------------------------------------------------------
# Distribution moment helpers — expected half-rectified means used by the
# AS-deployment overlay. Implemented via Monte Carlo because the input
# distribution is two-piece (asymmetric p10/p90 around p50) and the
# closed-form non-central rectified moment isn't materially simpler to
# write than the sample-and-mean approach.
# ---------------------------------------------------------------------------


def _expected_neg_rectified(dist: dict[str, Any] | None, n: int) -> list[float]:
    """``E[max(−X/100, 0)]`` per period, X ~ asymmetric two-piece dist."""
    return _expected_rectified(dist, n, sign=-1)


def _expected_pos_rectified(dist: dict[str, Any] | None, n: int) -> list[float]:
    """``E[max(+X/100, 0)]`` per period, X ~ asymmetric two-piece dist."""
    return _expected_rectified(dist, n, sign=+1)


def _expected_rectified(
    dist: dict[str, Any] | None, n: int, *, sign: int
) -> list[float]:
    if not dist or not _dist_has_spread(dist):
        # No distribution → use the deterministic p50 directly.
        p50 = _series_or_zero(dist, "p50", n)
        return [max(0.0, sign * p50[t] / 100.0) for t in range(n)]
    return [_rectified_mean(dist, t, sign) for t in range(n)]


def _expected_mean_clamped(
    dist: dict[str, Any] | None, n: int, *, lo: float, hi: float
) -> list[float]:
    """``E[X/100]`` clamped to ``[lo/100, hi/100]`` per period — the mean
    of the dist (since it's symmetric around p50 and our half-mass split
    keeps p50 the median, ``E[X] = p50`` only when the two halves have
    matching σ; for asymmetric we recover the expectation from a small MC
    draw)."""
    if not dist or not _dist_has_spread(dist):
        p50 = _series_or_zero(dist, "p50", n)
        return [max(lo / 100.0, min(hi / 100.0, p50[t] / 100.0)) for t in range(n)]
    return [_clamped_mean(dist, t, lo, hi) for t in range(n)]


def _dist_has_spread(dist: dict[str, Any]) -> bool:
    if not dist:
        return False
    spread = float(dist.get("spread_fraction") or 0.0)
    if spread > 1e-9:
        return True
    p10 = dist.get("p10") or []
    p90 = dist.get("p90") or []
    p50 = dist.get("p50") or []
    for i in range(min(len(p10), len(p50), len(p90))):
        if abs(float(p90[i]) - float(p50[i])) > 1e-9:
            return True
        if abs(float(p50[i]) - float(p10[i])) > 1e-9:
            return True
    return False


def _series_or_zero(
    dist: dict[str, Any] | None, key: str, n: int
) -> list[float]:
    if not dist:
        return [0.0] * n
    arr = dist.get(key) or []
    if len(arr) != n:
        return [0.0] * n
    return [float(v) for v in arr]


def _at(arr: list[Any] | None, t: int, fallback: float) -> float:
    """Safe indexing — returns ``fallback`` when ``arr`` is short or None.

    Saved scenarios occasionally arrive with distribution arrays whose
    length doesn't match the current period count (e.g. a resolution
    change that only resampled the LMP/AS forecasts). Defaulting to
    P50 instead of raising "list index out of range" lets the post-hoc
    overlay still run.
    """
    if not arr or t >= len(arr):
        return fallback
    return float(arr[t])


def _rectified_mean(dist: dict[str, Any], t: int, sign: int) -> float:
    """E[max(sign·X/100, 0)] for a single period via Monte Carlo."""
    import random

    samples = 200
    p50 = _at(dist.get("p50"), t, 0.0)
    p10 = _at(dist.get("p10"), t, p50)
    p90 = _at(dist.get("p90"), t, p50)
    shape = str(dist.get("shape") or "gaussian")
    acc = 0.0
    for _ in range(samples):
        x = _draw_two_piece(shape, p50, p10, p90, random.random, random.random)
        acc += max(0.0, sign * x / 100.0)
    return acc / samples


def _clamped_mean(
    dist: dict[str, Any], t: int, lo: float, hi: float
) -> float:
    import random

    samples = 200
    p50 = _at(dist.get("p50"), t, 0.0)
    p10 = _at(dist.get("p10"), t, p50)
    p90 = _at(dist.get("p90"), t, p50)
    shape = str(dist.get("shape") or "gaussian")
    acc = 0.0
    for _ in range(samples):
        x = _draw_two_piece(shape, p50, p10, p90, random.random, random.random)
        x = max(lo, min(hi, x))
        acc += x / 100.0
    return acc / samples


def _draw_two_piece(
    shape: str,
    p50: float,
    p10: float,
    p90: float,
    rand: Any,
    rand2: Any,
) -> float:
    """Sample from the same two-piece distribution the JS sampler uses.

    Mirrors :func:`drawPriceSample` in dashboard.js — two-piece (asymmetric)
    Gaussian / uniform / triangular, with each half scaled by its own
    quantile gap.
    """
    import math

    upper_spread = max(0.0, p90 - p50)
    lower_spread = max(0.0, p50 - p10)
    upper = rand() < 0.5
    spread = upper_spread if upper else lower_spread
    sign_dir = 1 if upper else -1
    if spread <= 0.0:
        return p50
    if shape == "uniform":
        half = spread / 0.8
        return p50 + sign_dir * rand() * half
    if shape == "triangular":
        w = spread / 0.553
        u = rand()
        return p50 + sign_dir * w * (1.0 - math.sqrt(1.0 - u))
    # Gaussian (default): half-normal via Box-Muller.
    sigma = spread / 1.282
    u1 = rand() or 1e-9
    u2 = rand2()
    z = abs(math.sqrt(-2.0 * math.log(u1)) * math.cos(2.0 * math.pi * u2))
    return p50 + sign_dir * z * sigma


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
        bess_daily_cycle_limit=_optional_fraction("bess_daily_cycle_limit"),
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
        enforce_reserve_soc_capacity=bool(
            policy_raw.get("enforce_reserve_soc_capacity", False)
        ),
    )
    return problem, policy


__all__ = [
    "available_as_products",
    "default_lmp_shape",
    "default_pwl_strategy",
    "default_scenario",
    "run_solve",
]
