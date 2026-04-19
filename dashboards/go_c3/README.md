# GO C3 — Dashboard

Live FastAPI dashboard: solve results, z-score breakdowns vs the
competition leaderboard, per-period device dispatch (DC vs AC vs
winner), branch flows, violations, reserve awards, and archived
prior runs. Imports surge once at startup, serves per-case JSON +
live re-solve / re-validate over `/api/*`.

## Install

```bash
uv pip install -e 'src/surge-py[dashboard]'   # fastapi, uvicorn, sse-starlette, httpx
```

## Run

```bash
# Foreground — good for active dev with --reload
uv run python -m dashboards.go_c3.server --reload

# Background (survives terminal close)
nohup uv run python -m dashboards.go_c3.server > /tmp/go_c3_server.log 2>&1 & disown

# → http://127.0.0.1:8787/
```

`--reload` watches `dashboards/go_c3/`, `benchmarks/go_c3/`,
`markets/go_c3/`, and the surge extension module
`src/surge-py/python/surge/_surge*.so`; re-running `cd src/surge-py &&
maturin develop` in another shell triggers a worker restart. The
top-nav badge (SURGE debug/release · sha) reports which build is
currently in-process.

Bind address defaults to `127.0.0.1`. Pass `--host 0.0.0.0` to expose
on LAN (no auth — only on trusted networks).

## API

| Endpoint | Purpose |
|---|---|
| `GET /api/health` | Loaded dylib path, build, mtime, size, git sha + dirty. |
| `GET /api/cases` | Index of every scenario in `datasets/` + `runs/baseline/`. |
| `GET /api/cases/{k}` | Per-case dashboard JSON (cached under `runs/dashboard-cache/`). |
| `GET /api/cases/{k}/log` | Live `solve.log` (tail up to 2 MB). |
| `GET /api/cases/{k}/archives` | Archived prior runs with timestamps + status. |
| `GET /api/cases/{k}/archives/{ts}` | Archived `run-report.json`. |
| `GET /api/cases/{k}/archives/{ts}/log` | Archived `solve.log`. |
| `POST /api/cases/{k}/solve` | Queue re-solve; body `{policy: GoC3Policy overrides}`. |
| `POST /api/cases/{k}/validate` | Queue re-validate via the official GO C3 validator. |
| `GET /api/jobs` | Recent jobs. |
| `GET /api/jobs/{id}` | Job detail + full log buffer. |
| `GET /api/jobs/{id}/stream` | SSE stream of `log` + `status` events. |

Jobs run in a 4-worker thread pool with a per-scenario lock so two
solves can't fight over the same workdir. Each solve archives the
prior run into `scenario_NNN/archive/{iso_ts}/`; newest 10 archives
are kept.

## Tabs

| Tab | Content |
|---|---|
| **Scores** | Leaderboard ranking (Surge vs competition), full z-score breakdown. |
| **Generators** | Per-period P/Q dispatch: DC SCUC, AC SCED, winner. Row click → offer curve. |
| **Loads** | Per-period load dispatch with dispatchable/fixed indicator. |
| **HVDC** | DC link P and Q schedules. |
| **Buses** | V/theta, P/Q injections, LMP — ours vs winner. |
| **Branches** | DC flow, AC flow, winner flow, limits, overloads, DC slack/penalty. |
| **AS Awards** | Per-device reserve awards by product type. |
| **AS System** | Zonal reserve requirements, provided, shortfalls, clearing prices. |
| **Contingencies** | Contingency definitions and outaged components. |
| **Violations** | Per-period bus P/Q balance and thermal violation breakdown. |
| **Log** | Live `solve.log` for the selected case. |

## Case discovery

The sidebar groups cases by network/division with collapsible SW0/SW1
sub-groups (collapsed by default). All 1358 cases (679 scenarios ×
SW0/SW1) are shown. Unsolved scenarios appear dimmed with
winner/leaderboard reference; when a switching mode has no
competition data, the other mode's results are shown with a banner.
