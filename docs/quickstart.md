# Quickstart

This is the shortest path to a working Surge build from this repository.

## 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

## 2. Install Native Dependencies

Surge needs pkg-config and SuiteSparse / KLU for AC workflows. HiGHS is the
default LP solver for OPF and needs to be installed as a system package.

```bash
# Ubuntu / Debian
sudo apt install pkg-config libsuitesparse-dev libclang-dev libhighs-dev

# Fedora
sudo dnf install pkgconf suitesparse-devel clang-devel

# macOS (Homebrew)
brew install pkg-config suite-sparse highs
```

For AC-OPF, install at least one NLP backend. Ipopt is the open-source path:

```bash
# Ubuntu / Debian
sudo apt install coinor-libipopt-dev

# macOS (Homebrew)
brew install ipopt
```

COPT 8.x is also supported as a commercial NLP backend. If `COPT_HOME` is set
when `surge-py` is built, the wheel bundles the Surge COPT NLP shim
automatically.

## 3. Build The CLI

```bash
cargo build --release --bin surge-solve
./target/release/surge-solve --help
```

Run a study:

```bash
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method acpf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method contingency --screening lodf
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst --method dc-opf --output json
```

Text output defaults to `--detail auto`: smaller cases keep the full tables,
while larger cases print a compact summary. Use `--detail full` to force the
detailed tables or `--detail summary` to force the compact view.

The example cases ship as `.surge.json.zst` (zstd-compressed Surge JSON). The
CLI and library APIs load this format transparently.

## 4. Build The Python Package From Source

Create a virtual environment (required on modern Python):

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin numpy pytest
```

Build and install the package in development mode:

```bash
cd src/surge-py
maturin develop --release
cd ../..
python -c "import surge; print(surge.version())"
```

To require a COPT-enabled Python build on release or CI builders:

```bash
cd src/surge-py
COPT_HOME=/path/to/copt80 SURGE_PY_REQUIRE_COPT_NLP_SHIM=1 maturin develop --release
```

### macOS Environment Notes

On macOS with Homebrew, Ipopt is discovered at runtime. Set the library path so
Surge can find `libipopt.dylib`:

```bash
export IPOPT_LIB_DIR=/opt/homebrew/lib          # Apple Silicon
export IPOPT_LIB_DIR=/usr/local/lib              # Intel Mac
```

Minimal example:

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
ac = surge.solve_ac_pf(net)
dc = surge.solve_dc_pf(net)
n1 = surge.analyze_n1_branch(net)

print(ac.converged)
print(dc.solve_time_secs)
print(n1.n_with_violations)
```

## 5. Use The Rust Crates Directly

```toml
[dependencies]
anyhow = "1"
surge-io = { path = "/path/to/surge/src/surge-io" }
surge-ac = { path = "/path/to/surge/src/surge-ac" }
```

```rust
use std::path::Path;

use anyhow::Result;
use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_io::load;

fn main() -> Result<()> {
    let net = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;
    let sol = solve_ac_pf(&net, &AcPfOptions::default())?;
    println!("iterations={} mismatch={:.2e}", sol.iterations, sol.max_mismatch);
    Ok(())
}
```

## Where To Go Next

- [support-compatibility.md](support-compatibility.md)
- [../examples/README.md](../examples/README.md)
- [tutorials/01-basic-power-flow.md](tutorials/01-basic-power-flow.md)
- [tutorials/05-python-api.md](tutorials/05-python-api.md)
- [tutorials/09-pandas-construction.md](tutorials/09-pandas-construction.md)
- [tutorials/10-subsystems.md](tutorials/10-subsystems.md)
- [tutorials/06-cli-reference.md](tutorials/06-cli-reference.md)
- [notebooks/README.md](notebooks/README.md)
