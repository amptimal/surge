# GO Competition Challenge 3 — Market Design

The market spec for the ARPA-E / NERC GO Competition Challenge 3.
Two-stage SCUC → AC SCED with full GO C3 penalty tensor (thermal,
voltage, bus balance, ramp, reserves — including reactive), security
screening, and the GO C3 solution export format.

Canonical implementation lives in Rust under ``src/surge-market/`` +
``src/surge-dispatch/``; the Python layer here is thin.

## Quick start

```python
from pathlib import Path
from markets.go_c3 import GoC3Policy, GoC3Problem, solve

report = solve(
    GoC3Problem.load("scenario.json"),
    workdir=Path("out/run-1"),
    policy=GoC3Policy(
        formulation="dc",
        ac_reconcile_mode="ac_dispatch",
        lp_solver="gurobi",
        nlp_solver="ipopt",
    ),
)
print(report["status"], report["artifacts"]["solution"])
```

The solve writes four files to *workdir*:

| File | Content |
|---|---|
| `solution.json` | GO C3 exported solution (what the validator scores). |
| `workflow-result.json` | Per-stage Rust workflow trace. |
| `run-report.json` | Status, timing, policy, step timings. |
| `solve.log` | Timestamped Python + (optional) Rust/solver log. |

## What this market declares

| Aspect | Value |
|---|---|
| **Problem schema** | GO C3 JSON (see [`problem.py`](problem.py)). |
| **Adapter** | Rust `surge_io::go_c3` — bus/device/branch mapping. |
| **Reserves** | 10-product GO C3 set (reg-up/down, spin, non-spin, ramp ↑/↓ on/off, reactive ↑/↓). |
| **Clearing** | DC SCUC MIP → AC SCED NLP (two-stage, commitment handoff). |
| **Pricing** | LP repricing for LMPs (optional; off by default). |
| **Security** | N-1 contingency screening via iterative flowgate cuts. |
| **Export** | `solution.json` in the GO C3 competition submission format. |

See [`config.py`](config.py) for the `MarketConfig` preset and
[`policy.py`](policy.py) for the full knob list.

## Knobs worth knowing

| Policy field | Purpose |
|---|---|
| `formulation` | `"dc"` or `"ac"` (SCUC formulation). |
| `ac_reconcile_mode` | `"ac_dispatch"` (two-stage), `"acpf"`, or `"none"` (SCUC only). |
| `allow_branch_switching` | SW1 mode — branch on/off becomes MIP decision. |
| `scuc_only` | Stop after SCUC; useful for MIP-tightening benchmarks. |
| `commitment_mip_rel_gap` / `commitment_time_limit_secs` | MIP optimality target and wall-time cap. |
| `commitment_mip_gap_schedule` | Time-varying gap schedule (step function). |
| `reactive_support_pin_factor` | Retry factor applied on NLP degeneracy fallback. |
| `capture_solver_log` | Tee Rust tracing + solver console into `solve.log` (~3000 lines). |

## What lives next to this market

* **Benchmark harness** at [`benchmarks/go_c3/`](../../benchmarks/go_c3/) —
  dataset fetchers, official GO C3 validator integration, winner /
  leaderboard reference data, suite runners, diff tools, and the
  scoring-replica `violations.py`.
* **Dashboard** at [`dashboards/go_c3/`](../../dashboards/go_c3/) —
  FastAPI server with per-case JSON views, live re-solve /
  re-validate, solve-log streaming.
