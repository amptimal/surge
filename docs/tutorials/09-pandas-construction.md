# Tutorial 09: Build Networks From pandas DataFrames

`surge.construction.from_dataframes(...)` is the pandas-native path into the
curated Python package. Use it when your model data already lives in DataFrames
and you want a real `surge.Network` without writing an intermediate file.

## Minimal Example

```python
import pandas as pd
import surge

buses = pd.DataFrame(
    [
        {"number": 1, "type": "Slack", "base_kv": 230.0, "name": "SWING"},
        {"number": 2, "type": "PV", "base_kv": 230.0, "name": "GEN"},
        {"number": 3, "type": "PQ", "base_kv": 230.0, "name": "LOAD", "pd_mw": 90.0, "qd_mvar": 30.0},
    ]
)
branches = pd.DataFrame(
    [
        {"from_bus": 1, "to_bus": 2, "r": 0.01, "x": 0.08, "rate_a": 250.0},
        {"from_bus": 2, "to_bus": 3, "r": 0.01, "x": 0.09, "rate_a": 250.0},
        {"from_bus": 1, "to_bus": 3, "r": 0.02, "x": 0.12, "rate_a": 250.0},
    ]
)
generators = pd.DataFrame(
    [
        {"bus": 1, "pg": 100.0, "pmax": 150.0, "qmax": 80.0, "qmin": -80.0},
        {"bus": 2, "pg": 40.0, "pmax": 80.0, "vs": 1.02},
    ]
)

net = surge.construction.from_dataframes(
    buses,
    branches,
    generators,
    base_mva=100.0,
    name="pandas-demo",
)

result = surge.solve_ac_pf(net)
print(result.converged, result.iterations)
```

## Schema

### `buses`

| Column | Required | Meaning | Default |
|---|---|---|---|
| `number` | yes | External bus number | none |
| `base_kv` | yes | Nominal kV | none |
| `type` | no | `PQ`, `PV`, `Slack`, or `Isolated` | `PQ` |
| `name` | no | Bus name | `""` |
| `pd_mw` | no | Real load in MW | `0.0` |
| `qd_mvar` | no | Reactive load in MVAr | `0.0` |
| `vm_pu` | no | Initial voltage magnitude | `1.0` |
| `va_deg` | no | Initial voltage angle in degrees | `0.0` |

### `branches`

| Column | Required | Meaning | Default |
|---|---|---|---|
| `from_bus` | yes | From-bus external number | none |
| `to_bus` | yes | To-bus external number | none |
| `r` | yes | Series resistance in per-unit | none |
| `x` | yes | Series reactance in per-unit | none |
| `b` | no | Total line charging susceptance in per-unit | `0.0` |
| `rate_a` | no | Continuous thermal rating in MVA | `0.0` |
| `tap` | no | Off-nominal tap ratio | `1.0` |
| `shift` | no | Phase shift in degrees | `0.0` |
| `circuit` | no | Parallel circuit identifier | `1` |

### `generators`

| Column | Required | Meaning | Default |
|---|---|---|---|
| `bus` | yes | Connected bus number | none |
| `pg` | yes | Scheduled real power in MW | none |
| `pmax` | no | Maximum MW output | `9999.0` |
| `pmin` | no | Minimum MW output | `0.0` |
| `qmax` | no | Maximum MVAr output | `9999.0` |
| `qmin` | no | Minimum MVAr output | `-9999.0` |
| `vs` | no | Voltage setpoint in per-unit | `1.0` |
| `machine_id` | no | Generator identifier | `"1"` |

## Error Semantics

- Missing required columns raise `KeyError`.
- Values that cannot be coerced to `int` or `float` raise `ValueError`.
- Duplicate bus numbers raise `surge.NetworkError`.
- Branches and generators that reference missing buses raise `surge.NetworkError`.

## Scope Limits

`from_dataframes(...)` is intentionally narrow for 0.1:

- It builds buses, branches, and generators only.
- It does not ingest separate load tables, shunt tables, HVDC assets, node-breaker topology, or market objects.
- Area and zone metadata are not part of this helper yet.
- Extra DataFrame columns are ignored by the constructor.

If you need the broader static model, build a `surge.Network` directly in code
or load a supported file format through `surge.load(...)`.
