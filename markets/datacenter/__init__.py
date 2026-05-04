# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""Datacenter operator market — single-site behind-the-meter SCUC.

Optimises the dispatch and commitment of a microgridded datacenter
load against an exogenous LMP forecast and (optional) AS price
forecasts. The asset stack is the canonical hyperscale-DC mix:
must-serve IT load, multiple curtailable IT tiers (inference /
training / batch with distinct VOLLs), solar, wind, BESS, fuel cell,
gas CT, diesel backup, and (optionally) nuclear baseload.

Quick start::

    from pathlib import Path
    from markets.datacenter import (
        AsProduct, BessSpec, CurtailableLoadTier, DataCenterPolicy,
        DataCenterProblem, FourCpSpec, ItLoadSpec, SiteSpec, SolarSpec,
        ThermalSpec, WindSpec, solve,
    )
    from surge.market import REG_UP, ECRS, NON_SPINNING

    problem = DataCenterProblem(
        period_durations_hours=[1.0] * 24,
        lmp_forecast_per_mwh=lmp_24h,
        site=SiteSpec(
            poi_limit_mw=1000.0,
            it_load=ItLoadSpec(
                must_serve_mw=[700.0] * 24,
                tiers=[
                    CurtailableLoadTier("training", 200.0, voll_per_mwh=40.0),
                    CurtailableLoadTier("research", 100.0, voll_per_mwh=5.0),
                ],
            ),
            bess=BessSpec(
                power_charge_mw=200.0, power_discharge_mw=200.0,
                energy_mwh=800.0,
            ),
            solar=SolarSpec(nameplate_mw=250.0, capacity_factors=solar_cf_24h),
            wind=WindSpec(nameplate_mw=150.0, capacity_factors=wind_cf_24h),
            fuel_cell=ThermalSpec(
                resource_id="fc", nameplate_mw=200.0, pmin_mw=20.0,
                fuel_price_per_mmbtu=8.0, heat_rate_btu_per_kwh=6500.0,
                min_up_h=2.0, min_down_h=1.0,
                qualified_as_products=("reg_up", "reg_down", "syn", "ecrs"),
            ),
            gas_ct=ThermalSpec(
                resource_id="ct", nameplate_mw=400.0, pmin_mw=50.0,
                fuel_price_per_mmbtu=4.0, heat_rate_btu_per_kwh=9500.0,
                vom_per_mwh=4.0, no_load_cost_per_hr=500.0,
                startup_cost_tiers=[{"max_offline_hours": 8.0, "cost": 4000.0}],
                min_up_h=4.0, min_down_h=4.0,
                co2_tonnes_per_mwh=0.45,
                qualified_as_products=("reg_up", "syn", "ecrs"),
            ),
            diesel=ThermalSpec(
                resource_id="diesel", nameplate_mw=50.0, pmin_mw=0.0,
                fuel_price_per_mmbtu=20.0, heat_rate_btu_per_kwh=10500.0,
                co2_tonnes_per_mwh=0.74,
                qualified_as_products=("nsyn",),
            ),
            four_cp=FourCpSpec(
                period_indices=[16, 17, 18, 19],     # late-afternoon flag
                charge_per_mw=10000.0,                # ≈ $40k/MW-yr ÷ 4
            ),
        ),
        as_products=[
            AsProduct(REG_UP, [8.0] * 24),
            AsProduct(ECRS, [6.0] * 24),
            AsProduct(NON_SPINNING, [3.0] * 24),
        ],
    )
    report = solve(problem, Path("out/datacenter"), policy=DataCenterPolicy())
    print(report["extras"]["pl_summary"])

The market builds a 1-bus surge.Network with all of the above and
runs a SCUC MIP that co-optimises commitment, dispatch, AS awards,
and (if enabled) coincident-peak transmission demand charges.

See :mod:`markets.datacenter.problem` for the full asset-spec details.
"""

from .policy import COMMITMENT_MODES, DataCenterPolicy, PERIOD_COUPLINGS
from .problem import (
    AsProduct,
    BessSpec,
    BESS_RESOURCE_ID,
    CurtailableLoadTier,
    DataCenterProblem,
    FourCpSpec,
    GRID_EXPORT_RESOURCE_ID,
    GRID_IMPORT_RESOURCE_ID,
    ItLoadSpec,
    NuclearSpec,
    SOLAR_RESOURCE_ID,
    SiteSpec,
    SolarSpec,
    ThermalSpec,
    WIND_RESOURCE_ID,
    WindSpec,
)
from .solve import solve
from .export import extract_pl_report, extract_pl_report_from_sequence

__all__ = [
    "DataCenterPolicy",
    "DataCenterProblem",
    "solve",
    "AsProduct",
    "BessSpec",
    "BESS_RESOURCE_ID",
    "COMMITMENT_MODES",
    "CurtailableLoadTier",
    "FourCpSpec",
    "GRID_EXPORT_RESOURCE_ID",
    "GRID_IMPORT_RESOURCE_ID",
    "ItLoadSpec",
    "NuclearSpec",
    "PERIOD_COUPLINGS",
    "SOLAR_RESOURCE_ID",
    "SiteSpec",
    "SolarSpec",
    "ThermalSpec",
    "WIND_RESOURCE_ID",
    "WindSpec",
    "extract_pl_report",
    "extract_pl_report_from_sequence",
]
