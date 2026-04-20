# Surge

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20NC%201.0.0-blue)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-repo-informational)](docs/index.md)

Surge is a Rust-native power-systems and electricity-market engine. It covers
the full steady-state analysis stack — AC/DC power flow, HVDC, contingency
analysis, OPF, transfer capability, and node-breaker topology — and a unified
SCED/SCUC dispatch kernel with a canonical market-formulation layer on top:
reserve products, multi-stage DC-SCUC → AC-SCED workflows, offer assembly, and
adapters for real market data.

The repository exposes three supported interfaces:

- Rust crates in a workspace rooted here
- the `surge-solve` CLI from `src/surge-bindings`
- the `surge` Python package built from `src/surge-py`

Surge is source-available under PolyForm Noncommercial 1.0.0. Commercial use
requires a separate license from Amptimal. See [LICENSE](LICENSE),
[COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md), and
[license-notes.md](docs/license-notes.md).

## Markets

The `markets/` folder is where market simulations built on
a declarative spec — a `Policy`, a `Problem`, and a `solve` entry point —
built on the canonical dispatch and market layers in `surge-dispatch` and
`surge-market`. Input and output flow through native Surge types
(`surge.Network`, the typed `DispatchRequest`, `DispatchResult`,
`MarketConfig`)

| Market | What it clears | Topology | Workflow |
|---|---|---|---|
| [`markets/rto/`](markets/rto/README.md) | ISO day-ahead energy + ancillary services with LMPs | Any `surge.Network` | DC SCUC MIP + LMP repricing LP |
| [`markets/battery/`](markets/battery/README.md) | Single-site price-taker BESS against an LMP forecast | 1-bus in-memory | Single-stage time-coupled LP |
| [`markets/go_c3/`](markets/go_c3/README.md) | GO Competition Challenge 3 scenarios | GO C3 JSON → adapter | DC SCUC MIP → AC SCED NLP |

Public Python surface:

```python
from markets.rto     import RtoPolicy,     RtoProblem,     solve
from markets.battery import BatteryPolicy, BatteryProblem, solve
from markets.go_c3   import GoC3Policy,    GoC3Problem,    solve
```

See [markets/README.md](markets/README.md) for the market contract, the full
`DispatchRequest` builder, and how to add a new market by copying
[`markets/_template/`](markets/_template/README.md).

## Workspace Map

Grouped by role:

**Foundation**
| Crate | Role |
|---|---|
| `surge-network` | Shared network model and equipment/domain types |
| `surge-solution` | Shared result contracts and replay-friendly solved outputs |
| `surge-sparse` | Sparse matrix and factorization helpers |
| `surge-io` | Canonical file import/export APIs |
| `surge-topology` | Node-breaker rebuild and topology processing |

**Steady-state and transfer**
| Crate | Role |
|---|---|
| `surge-dc` | DC power flow, PTDF/LODF/OTDF, and prepared DC studies |
| `surge-ac` | AC Newton-Raphson, fast-decoupled power flow, and AC-side controls |
| `surge-hvdc` | HVDC power flow for point-to-point links and explicit DC grids |
| `surge-transfer` | ATC/AFC and reusable transfer capability studies |

**Optimization and security**
| Crate | Role |
|---|---|
| `surge-opf` | DC-OPF, AC-OPF, and SCOPF |
| `surge-contingency` | N-1, N-2, screening, and follow-on contingency workflows |

**Dispatch and markets**
| Crate | Role |
|---|---|
| `surge-dispatch` | Unified SCED/SCUC kernel — DC or AC, period-by-period or time-coupled, with reserves, N-1 security screening, and SCED-AC Benders |
| `surge-market` | Canonical market-formulation layer — reserve catalogues, offer assembly, startup/shutdown trajectories, multi-stage workflows, AC SCED setup, retry/refinement runtime, GO C3 adapter |

**Interfaces**
| Crate | Role |
|---|---|
| `surge-bindings` | `surge-solve` CLI |
| `surge-py` | Python bindings and typed package layer |

## Current Capabilities

- AC power flow with Newton-Raphson and fast-decoupled methods
- DC power flow, PTDF, LODF, OTDF, and batched N-2 sensitivities
- HVDC solves for point-to-point links and explicit VSC/LCC DC-network models
- Branch and generator contingency analysis with screening
- DC-OPF, AC-OPF, and SCOPF
- Transfer capability workflows including NERC-style ATC
- Unified SCED/SCUC dispatch — DC or AC, period-by-period or time-coupled,
  with reserve products, N-1 security screening, and SCED-AC Benders
- Multi-stage market workflows (DC SCUC → AC SCED) with canonical
  retry/refinement runtime and typed `DispatchRequest` builder
- Reference markets: ISO day-ahead (RTO), single-site price-taker BESS,
  GO Competition Challenge 3
- Node-breaker topology rebuild and mapping
- Rust, CLI, and Python access to the same core analysis stack

## Quick Start

### Python — analysis

Install the prebuilt package from PyPI:

```bash
pip install surge-py
```

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
ac = surge.solve_ac_pf(net)
print(ac.converged, ac.iterations, ac.max_mismatch)
```

### Python — a market solve

A 24-hour single-site battery revenue-ceiling run against an LMP forecast:

```python
from pathlib import Path
from markets.battery import BatteryPolicy, BatteryProblem, SiteSpec, solve

lmp = [25, 22, 20, 18, 20, 25, 30, 40, 55, 60,
       65, 70, 70, 68, 65, 60, 55, 50, 60, 75,
       80, 70, 50, 35]

problem = BatteryProblem(
    period_durations_hours=[1.0] * 24,
    lmp_forecast_per_mwh=lmp,
    site=SiteSpec(
        poi_limit_mw=50.0,
        bess_power_charge_mw=25.0,
        bess_power_discharge_mw=25.0,
        bess_energy_mwh=100.0,
        bess_charge_efficiency=0.90,
        bess_discharge_efficiency=0.98,
        bess_initial_soc_mwh=50.0,
    ),
)
report = solve(problem, Path("out/ceiling"), policy=BatteryPolicy())
print(report["revenue_summary"])
```

A minimal ISO day-ahead clearing on `surge.case14()` is in
[`markets/rto/README.md`](markets/rto/README.md); the GO Competition Challenge 3
adapter (SCUC MIP → AC SCED NLP) is in
[`markets/go_c3/README.md`](markets/go_c3/README.md).

### CLI

Building the CLI requires a [Rust toolchain](https://rustup.rs/) (stable 1.87+)
and the native dependencies listed in [Build Notes](#build-notes). If you don't
have Rust installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Then build and run:

```bash
cargo build --release --workspace --exclude surge-py
./target/release/surge-solve --help
```

A few common studies:

```bash
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method acpf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method contingency --screening lodf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method dc-opf --output json
```

### Rust

```rust
use std::path::Path;

use anyhow::Result;
use surge_ac::{solve_ac_pf, AcPfOptions};
use surge_io::load;

fn main() -> Result<()> {
    let net = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;
    let sol = solve_ac_pf(&net, &AcPfOptions::default())?;
    println!("iterations={} mismatch={:.2e}", sol.iterations, sol.max_mismatch);
    Ok(())
}
```

Building `surge-py` from source, the `./py` / `./py-build` contributor
launchers, and the COPT NLP shim are covered in
[docs/quickstart.md](docs/quickstart.md).

## File And Data Support

At the repository level, Surge currently includes:

- canonical load/save paths for MATPOWER, PSS/E RAW, XIIDM, UCTE, OpenDSS,
  GE EPC, Surge JSON, and Surge BIN
- additional import/export modules for CGMES/CIM, PSS/E RAWX, DYR, sequence
  data, IEEE CDF, and related sidecar formats
- packaged native example cases under [examples/README.md](examples/README.md)

When a format has specialized behavior, prefer the format-specific APIs in
`surge-io` over secondary prose.

## Build Notes

The shortest path to a working build is in
[docs/quickstart.md](docs/quickstart.md). Supported versions, platforms, and
native dependency expectations live in
[docs/support-compatibility.md](docs/support-compatibility.md).

Common native requirements:

## Documentation

- [docs/quickstart.md](docs/quickstart.md)
- [docs/data-model.md](docs/data-model.md)
- [docs/support-compatibility.md](docs/support-compatibility.md)
- [docs/architecture.md](docs/architecture.md)
- [docs/glossary.md](docs/glossary.md)
- [docs/references.md](docs/references.md)
- [docs/performance.md](docs/performance.md)
- [docs/tutorials/](docs/tutorials/)
- [docs/notebooks/README.md](docs/notebooks/README.md)
- [docs/crates/](docs/crates/)
- [markets/README.md](markets/README.md)
- [examples/README.md](examples/README.md)

## Repository Process

- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [RELEASING.md](RELEASING.md)

Public docs in this repository should only make claims that can be traced to
the current codebase.
