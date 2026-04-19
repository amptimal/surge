# RTO Day-Ahead Market

Single-zone or multi-zone ISO-style day-ahead clearing. Sources its
input (network + forecasts) and emits its output (dispatch + LMPs +
AS awards + settlement) through native Surge data types.

## What this market declares

| Aspect | Value |
|---|---|
| **Problem schema** | In-memory: `surge.Network` + per-bus load MW arrays + per-resource renewable caps + reserve requirements. On-disk: three CSVs via `RtoProblem.from_csvs`. |
| **Formulation** | DC, time-coupled (ramp + min up/down across periods). |
| **Clearing** | SCUC MIP; optional LMP-repricing LP with commitment pinned. |
| **AS products** | Regulation up, regulation down, spinning, non-spinning (no reactive, no ramp reserves). |
| **Penalty tensor** | Bus-balance = VOLL ($9 000/MWh default); thermal overload = $5 000/MWh; reserve shortfall = $1 000/MWh. |
| **Pricing** | LMPs with `mec`/`mcc`/`mlc` decomposition from the solver's bus shadow prices. |
| **Export** | `settlement.json` — per-bus LMPs per period, per-resource energy awards + payments, per-zone AS awards + clearing prices, system totals. |

## Quick start

```python
from pathlib import Path
import surge
from markets.rto import RtoPolicy, RtoProblem, solve
from surge.market import ZonalRequirement

network = surge.case14()
problem = RtoProblem.from_dicts(
    network,
    period_durations_hours=[1.0] * 24,
    load_forecast_mw={
        # Scale bus 4's baseline 47.8 MW into a morning/evening peak curve.
        4: [max(25.0, 60.0 * (0.7 + 0.3 * __import__("math").sin((t - 6) * 3.14 / 12))) for t in range(24)],
    },
    reserve_requirements=[
        ZonalRequirement(
            zone_id=1, product_id="reg_up",
            requirement_mw=5.0, per_period_mw=[5.0] * 24,
        ),
        ZonalRequirement(
            zone_id=1, product_id="syn",
            requirement_mw=10.0, per_period_mw=[10.0] * 24,
        ),
    ],
)

report = solve(
    problem, Path("out/dam"),
    policy=RtoPolicy(lp_solver="highs", commitment_mode="optimize"),
)
print(report["status"], report["extras"]["settlement_summary"])
```

## Policy knobs (`RtoPolicy`)

| Field | Default | Purpose |
|---|---|---|
| `lp_solver` | `"highs"` | LP / MIP solver (`"highs"`, `"gurobi"`). |
| `commitment_mode` | `"optimize"` | `"optimize"` (SCUC MIP), `"all_committed"` (LP, every unit online), or `"fixed_initial"` (MIP-free, pinned to initial status). |
| `mip_gap` | `1e-3` | MIP optimality gap. |
| `time_limit_secs` | `None` | MIP time limit. |
| `run_pricing` | `True` | Run LMP repricing (via `runtime.run_pricing`). |
| `voll_per_mwh` | `9 000` | Power-balance violation cost. |
| `thermal_overload_cost_per_mwh` | `5 000` | Thermal-limit violation cost. |
| `reserve_shortfall_cost_per_mwh` | `1 000` | Product demand curve price. |

## Input shape

`RtoProblem.from_dicts(...)` for tests / notebooks:

```python
RtoProblem.from_dicts(
    network,                             # surge.Network (any case)
    period_durations_hours=[1.0] * 24,   # list[float]
    load_forecast_mw={4: [...], 5: [...]},   # bus_number → per-period MW
    renewable_caps_mw={"wind_1": [...]}, # resource_id → per-period MW cap; normalised to capacity factors via the generator's pmax_mw
    reserve_requirements=[...],          # list[ZonalRequirement]
    initial_commitment={"gen_1_1": True}, # optional
    previous_dispatch_mw={"gen_1_1": 150.0},  # optional, for ramp init
    energy_offers=[...],                 # list[GeneratorOfferSchedule] (optional)
    reserve_offers=[...],                # list[GeneratorReserveOfferSchedule] (optional)
)
```

> **Renewable caps.** Each `resource_id` in `renewable_caps_mw` must
> exist in the network and have `pmax_mw > 0`. The per-period MW
> caps are normalised to capacity factors (`caps_mw / pmax_mw`) before
> being sent to the solver as a `RenewableProfile`. A missing
> resource or a zero `pmax_mw` raises `ValueError` at `build_request`
> time rather than silently dropping the profile.

`RtoProblem.from_csvs(...)` for disk-first workflows — three small CSVs:

| File | Required columns |
|---|---|
| `load.csv` | `bus_number, period, value_mw` |
| `renewable.csv` (optional) | `resource_id, period, cap_mw` |
| `reserves.csv` (optional) | `zone_id, product_id, period, requirement_mw` (+ optional `shortfall_cost_per_unit`) |

When no per-resource `energy_offers` are supplied, the solver uses the
`surge.Network`'s generator cost coefficients (`c2`, `c1`, `c0`).

## Output shape

`solve` writes to *workdir*:

| File | Content |
|---|---|
| `run-report.json` | Status, timing, policy, per-period summary, paths to other artifacts. |
| `settlement.json` | Per-period per-bus LMPs (`lmp`, `mec`, `mcc`, `mlc`), per-resource energy awards + payments, per-zone AS awards + clearing prices, system totals (energy / AS payment, congestion rent, shortfall penalty, production cost). |
| `dispatch-result.json` | Native `DispatchResult.to_dict()` — for violation reports, dashboards. |

The returned dict includes `report["extras"]["settlement_summary"]` with the four
top-line $ totals.

## Scope

Phase 1 (this release):

- **DAM only.** Real-time SCED with look-ahead is deferred to phase 5.
- **DC formulation only.** AC reconcile deferred.
- **4-product AS.** Ramp and reactive reserves deferred.
- **No N-1 security screening** in the default path. Enable manually by
  passing custom commitment options (advanced).

## What lives next to this market

- Dashboard (future): `dashboards/rto/` will host an interactive
  settlement / LMP heatmap view.
- Harness (future): `benchmarks/rto/` will carry reference cases and
  comparison tooling.
