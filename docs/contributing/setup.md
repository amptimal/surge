# Developer Setup

This guide covers contributor setup for this repository. For end-user build
instructions, start with [../quickstart.md](../quickstart.md) and
[../support-compatibility.md](../support-compatibility.md).

## Required Tools

- Rust stable 1.87+
- Git
- C/C++ compiler (`gcc`, `clang`, or MSVC)
- SuiteSparse / KLU development libraries
- Python 3.12 through 3.14 for the Python package and tests

Optional but useful:

- Ipopt for AC-OPF
- JupyterLab for notebook work

## Platform-Specific Setup

### Ubuntu / Debian

```bash
sudo apt install build-essential pkg-config libclang-dev \
    libsuitesparse-dev coinor-libipopt-dev libopenblas-dev libhighs-dev
```

### Fedora

```bash
sudo dnf install pkgconf clang-devel suitesparse-devel coin-or-Ipopt-devel
```

### macOS (Homebrew)

```bash
brew install pkg-config suite-sparse ipopt highs
```

For runtime solver discovery on macOS, set `IPOPT_LIB_DIR` in your shell
profile:

```bash
export IPOPT_LIB_DIR=/opt/homebrew/lib   # Apple Silicon
export IPOPT_LIB_DIR=/usr/local/lib      # Intel Mac
```

## Clone And Bootstrap

```bash
git clone https://github.com/amptimal/surge
cd surge
```

## Build The Workspace

```bash
cargo build --release --workspace --exclude surge-py
cargo build --release --bin surge-solve
```

`surge-py` should be built with maturin rather than through a full workspace
Cargo build.

## Environment Variables

| Variable | Purpose | Example |
|---|---|---|
| `HIGHS_LIB_DIR` | Directory containing `libhighs.{so,dylib}` | `/opt/homebrew/lib` |
| `IPOPT_LIB_DIR` | Directory containing `libipopt.{so,dylib}` | `/opt/homebrew/lib` |
| `GUROBI_HOME` | Gurobi install root (runtime) | `/opt/gurobi1100/linux64` |
| `COPT_HOME` | COPT install root (runtime and wheel-build bundling) | `/opt/copt80` |
| `SURGE_PY_REQUIRE_COPT_NLP_SHIM` | Fail `surge-py` builds unless the packaged COPT NLP shim is bundled | `0` or `1` |
| `SURGE_COPT_NLP_SHIM_PATH` | Override the runtime path to the standalone COPT NLP shim | `/path/to/libsurge_copt_nlp.dylib` |
| `SURGE_TEST_DATA` | Override test data directory | `../surge-bench/instances` |
| `CARGO_TARGET_DIR` | Override Cargo build output dir | `/tmp/surge-target` |

## Python Package Development

Use a virtual environment:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pandas scipy pyyaml numpy matplotlib
```

Build and install in development mode:

```bash
cd src/surge-py
maturin develop --release
cd ../..
python -c "import surge; print(surge.version())"
```

Build a wheel:

```bash
cd src/surge-py
maturin build --release
pip install ../../target/wheels/surge_py-*.whl
```

To require a COPT-enabled wheel or development install:

```bash
COPT_HOME=/path/to/copt80 SURGE_PY_REQUIRE_COPT_NLP_SHIM=1 \
  maturin develop --release
```

## Test And Review Loop

Baseline repo checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --exclude surge-py
```

Python tests:

```bash
cd src/surge-py
maturin develop --release
cd ../..
pytest src/surge-py/tests/ -x -v --tb=short
```

Some extended tests depend on case libraries from the separate `surge-bench`
repository and skip gracefully when that data is absent.

## Optional Notebook Work

Notebook examples live under [../notebooks](../notebooks). If you edit public
notebooks, keep them aligned with the current Python package surface and remove
placeholder behavior.

## Optional `surge-bench` Checkout

Cross-tool validation and large-case evidence live primarily in the separate
`surge-bench` repository.

```bash
cd ../surge-bench
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```
