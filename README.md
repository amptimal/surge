# Surge

[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20NC%201.0.0-blue)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-repo-informational)](docs/index.md)

Surge is a Rust-native power-systems analysis workspace for transmission-focused
steady-state studies. The repository covers network interchange, AC/DC power
flow, HVDC coupling, sensitivities, contingency analysis, optimal power flow,
transfer capability workflows, topology rebuild, a CLI, and a Python package.

This repository exposes three supported interfaces:

- Rust crates in a workspace rooted here
- the `surge-solve` CLI from `src/surge-bindings`
- the `surge` Python package built from `src/surge-py`

Surge is source-available under PolyForm Noncommercial 1.0.0. Commercial use
requires a separate license from Amptimal. See [LICENSE](LICENSE),
[COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md), and
[license-notes.md](docs/license-notes.md).

## Workspace Map

| Crate | Role |
|---|---|
| `surge-network` | Shared network model and equipment/domain types |
| `surge-solution` | Shared result contracts and replay-friendly solved outputs |
| `surge-sparse` | Sparse matrix and factorization helpers |
| `surge-io` | Canonical file import/export APIs |
| `surge-topology` | Node-breaker rebuild and topology processing |
| `surge-dc` | DC power flow, PTDF/LODF/OTDF, and prepared DC studies |
| `surge-ac` | AC Newton-Raphson, fast-decoupled power flow, and AC-side controls |
| `surge-hvdc` | HVDC power flow for point-to-point links and explicit DC grids |
| `surge-contingency` | N-1, N-2, screening, and follow-on contingency workflows |
| `surge-opf` | DC-OPF, AC-OPF, and SCOPF |
| `surge-transfer` | ATC/AFC and reusable transfer capability studies |
| `surge-bindings` | `surge-solve` CLI |
| `surge-py` | Python bindings and typed package layer |

## Current Capabilities

- AC power flow with Newton-Raphson and fast-decoupled methods
- DC power flow, PTDF, LODF, OTDF, and batched N-2 sensitivities
- HVDC solves for point-to-point links and explicit VSC/LCC DC-network models
- Branch and generator contingency analysis with screening options
- DC-OPF, AC-OPF, and SCOPF
- Transfer capability workflows including NERC-style ATC
- Node-breaker topology rebuild and mapping
- Rust, CLI, and Python access to the same core analysis stack

## Quick Start

### Python

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

To build from source instead, see [Building from source](#building-from-source)
below.

### CLI

Building the CLI requires a [Rust toolchain](https://rustup.rs/) (stable 1.87+)
and native dependencies listed in [Build Notes](#build-notes). If you don't have
Rust installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Then build and run:

```bash
cargo build --release --workspace --exclude surge-py
./target/release/surge-solve --help
```

Run a few common studies:

```bash
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method acpf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method contingency --screening lodf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method dc-opf --output json
```

### Building from source

Building the Python package from source requires a
[Rust toolchain](https://rustup.rs/) (stable 1.87+) and native dependencies
listed in [Build Notes](#build-notes).

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin numpy
cd src/surge-py
maturin develop --release
cd ../..
python -c "import surge; print(surge.version())"
```

If `COPT_HOME` points to a COPT 8.x install when the package is built, the
Python package bundles the Surge COPT NLP shim into the wheel and configures it
automatically at import time. Python users still need a working COPT runtime
installation and license to run `nlp_solver="copt"`.

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

The shortest path to a working build is documented in
[docs/quickstart.md](docs/quickstart.md). Supported versions, platforms, and
native dependency expectations live in
[docs/support-compatibility.md](docs/support-compatibility.md).

Common native requirements:

- Rust stable 1.87+
- SuiteSparse / KLU development libraries for AC workflows
- HiGHS C library for `surge-opf` (install via package manager or set `HIGHS_LIB_DIR`)
- Ipopt C library for open-source AC-OPF (install via package manager or set `IPOPT_LIB_DIR`)
- COPT 8.x if you want the commercial AC-OPF backend; `surge-py` wheels built
  with `COPT_HOME` bundle the Surge NLP shim automatically

> **Note:** `pip install highspy` and `pip install cyipopt` do **not** provide
> the C shared libraries Surge needs. Install HiGHS and Ipopt via your system
> package manager (`brew install highs ipopt` /
> `apt install libhighs-dev coinor-libipopt-dev`).

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
- [examples/README.md](examples/README.md)

## Repository Process

- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [RELEASING.md](RELEASING.md)

Cross-tool validation and benchmark harnesses live primarily in the separate
`surge-bench` repository. Public docs in this repository should only make
claims that can be traced to the current codebase or to maintained evidence in
that benchmark repository.
