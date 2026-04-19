# GO C3 — Benchmark Harness

Pairs with [`markets/go_c3/`](../../markets/go_c3/) to add the
machinery for running, scoring, and comparing solutions against the
official GO Competition Challenge 3 data.

## What's here

| Module | Purpose |
|---|---|
| `runner.py` | `solve_baseline_scenario`, `validate_baseline_solution`, `run_suite`, `solve_sced_fixed` — suite-level wrappers over `markets.go_c3.solve`. |
| `datasets.py` / `manifests.py` / `paths.py` | GO C3 dataset discovery + on-demand unpack into `target/benchmarks/go-c3/datasets/`. |
| `validator.py` | Official GO C3 validator integration (auto-installs the validator venv). |
| `references.py` / `leaderboard.py` | Winner submissions and competition leaderboard data. |
| `violations.py` | Pi-model violation replica — matches the official validator penny-for-penny. |
| `commitment.py` | `extract_reference_schedule` — pin SCED commitment to a winner solution. |
| `compare.py` / `comparator.py` / `detail_compare.py` / `q_term_diff.py` / `inspector.py` / `ledger.py` | Diff tools. |
| `winner_roundtrip*.py` | Replay a winner's solution through Surge's validator. |
| `cli.py` | Developer CLI (`python -m benchmarks.go_c3.cli ...`). |

## Quick start

```python
from benchmarks.go_c3.cli import _locate_scenario, default_cache_root
from benchmarks.go_c3.runner import solve_baseline_scenario, validate_baseline_solution
from benchmarks.go_c3.validator import ensure_validator_environment
from markets.go_c3 import GoC3Policy

cache_root = default_cache_root()
_, scenario = _locate_scenario("event4_73", "D2", 911, cache_root=cache_root)

policy = GoC3Policy(
    formulation="dc",
    ac_reconcile_mode="ac_dispatch",
    lp_solver="gurobi",
    nlp_solver="ipopt",
    commitment_mip_rel_gap=0.0001,
    commitment_time_limit_secs=600.0,
)

report = solve_baseline_scenario(scenario, cache_root=cache_root, policy=policy)
print(report["status"], report.get("violation_summary"))

validator_env = ensure_validator_environment(cache_root=cache_root)
validation = validate_baseline_solution(
    scenario, cache_root=cache_root, validator_env=validator_env, policy=policy,
)
print(validation["validation_summary"])
```

## Output layout

Each solve writes to
`{cache_root}/runs/baseline/{dataset}/{division}/{sw0|sw1}/scenario_{id}/`:

| File | Content |
|---|---|
| `solution.json` | GO C3 exported solution (from the market solve). |
| `workflow-result.json` | Per-stage Rust workflow trace. |
| `run-report.json` | Harness-enriched report (status, timings, dispatch summary, violation summary, load-value summary, policy). |
| `solve.log` | Timestamped solve log. |
| `dispatch-result.json` / `dc-dispatch-result.json` | Final AC SCED + intermediate DC SCUC dispatch payloads (for the dashboard). |
| `violation-report.json` | Pi-model violation breakdown (matches validator). |
| `dispatch-load-value-report.json` / `dc-dispatch-load-value-report.json` | Per-consumer served-value reports. |
| `archive/{iso_ts}/` | Prior runs, newest 10 retained. |

Validator runs write to
`{cache_root}/runs/validator-baseline/.../scenario_NNN/` (pop
validation to `runs/validator-pop/`, sced-fixed to `runs/sced-fixed/`).

## Available datasets

| Network | Divisions | Scenarios | Notes |
|---|---|---|---|
| 73-bus | D1, D2, D3 | 88 | Primary dev/test target |
| 617-bus | D1, D2, D3 | 175 | |
| 1576-bus | D1, D3 | 48 | |
| 2000-bus | D1, D2, D3 | 39 | |
| 4224-bus | D1, D2, D3 | 72 | |
| 6049-bus | D1, D2, D3 | 78 | |
| 6717-bus | D1, D2, D3 | 57 | |
| 8316-bus | D1, D2, D3 | 116 | 3 dataset archives |
| 23643-bus | D1, D2, D3 | 6 | |

Each scenario has SW0 (non-switching) and SW1 (branch switching)
variants — 1358 total cases.

## Tests

```bash
uv run pytest tests/go_c3/ -x -v --tb=short
```

## Validator cache backups

Avoid re-downloading winner submissions (~9.4 GB):

```bash
cd target/benchmarks && tar xzf go-c3-dashboard-cache-backup.tar.gz
cd target/benchmarks && tar xzf go-c3-reference-submissions-backup.tar.gz
```
