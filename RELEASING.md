# Releasing Surge

This file covers the manual release process for Surge.

## Release Model

A Surge release is a coordinated event that includes:

- GitHub source release
- crates.io publication for the Rust library crates that are part of the
  supported release surface
- PyPI publication for the Python package and release wheels

## Before You Start

- Confirm branch protection is enabled on `main`.
- Confirm the release version is correct in Cargo and Python metadata.
- Confirm the public docs match the release you are about to ship.
- Confirm the support matrix in [docs/support-compatibility.md](docs/support-compatibility.md).

## Manual Pre-Release Checklist

Run the checks that apply to the release:

```bash
cargo fmt --all -- --check
cargo check -p surge-py
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --exclude surge-py
cargo build --release --workspace --exclude surge-py
pip install maturin pytest pandas scipy pyyaml jupyter nbconvert
cd src/surge-py
COPT_HOME=/path/to/copt80 SURGE_PY_REQUIRE_COPT_NLP_SHIM=1 maturin develop --release
cd ../..
pytest src/surge-py/tests/ -x -v --tb=short
python scripts/render_public_surface.py --cli-bin target/release/surge-solve
```

Manually verify:

- CLI examples in [README.md](README.md) and [docs/tutorials/06-cli-reference.md](docs/tutorials/06-cli-reference.md)
- Python examples in [README.md](README.md), [docs/tutorials/05-python-api.md](docs/tutorials/05-python-api.md), and [docs/notebooks/README.md](docs/notebooks/README.md)
- public notebooks execute cleanly under the supported Python versions you are releasing
- local documentation links
- Python support range 3.12 through 3.14
- generated surface docs are up to date

## Packaging Steps

### Rust

- run the publish commands from the workspace root
- update Cargo metadata if needed
- for the initial `0.1.0` crates.io publication, do not treat
  `cargo package --workspace` as a meaningful preflight for the whole graph:
  once Cargo packages a crate, internal workspace dependencies are resolved by
  version from crates.io rather than by local path, so unpublished dependent
  crates will fail until their prerequisites exist on the index
- publish these crates to crates.io in dependency order:
  `surge-sparse`, `surge-network`, `surge-solution`, `surge-topology`,
  `surge-ac`, `surge-dc`, `surge-io`, `surge-hvdc`, `surge-opf`,
  `surge-contingency`, `surge-transfer`, and `surge-bindings`
- after each dependency layer is live on crates.io, run
  `cargo package -p <crate> --allow-dirty --no-verify` for the next crate
  before publishing it
- if you need a full publishability dry-run before the first public release,
  use a staged local registry or equivalent publish simulation rather than the
  workspace-wide command above
- do not publish `surge-py` to crates.io; it is distributed as a Python
  wheel via PyPI
- `surge-bindings` (the `surge-solve` CLI) is published to crates.io so
  users can install via `cargo install surge-bindings`

### Python

- build generic public wheels from the manual wheel workflow (`artifact_profile=public`)
- if desired, build targeted `x86-64-v4` wheels as GitHub Release artifacts (`artifact_profile=targeted`)
- on wheel builders that are supposed to ship COPT-enabled Python artifacts, set
  `COPT_HOME=/path/to/copt80` and `SURGE_PY_REQUIRE_COPT_NLP_SHIM=1` so the
  wheel build fails closed if the packaged COPT NLP shim is not bundled
- run `python scripts/check_optional_solver_linkage.py src/surge-py/dist/*.whl`
- verify wheel install and import
- verify the wheel contains `surge/libsurge_copt_nlp.{so,dylib}` (or
  `surge/surge_copt_nlp.dll` on Windows) when you intend to ship COPT-enabled
  wheels
- verify an installed wheel auto-configures `SURGE_COPT_NLP_SHIM_PATH` and can
  run `solve_ac_opf(..., runtime=surge.AcOpfRuntime(nlp_solver="copt"))` on a
  smoke case with only `COPT_HOME` set
- publish only the generic public wheels to PyPI
- attach targeted `x86-64-v4` wheels to the GitHub release entry instead of PyPI

## GitHub Release

- create the release tag
- create the GitHub release entry
- attach or link the relevant release artifacts if needed
- attach targeted wheel artifacts there if you built them

## Post-Release Checks

- confirm crates.io metadata and crate dependencies look correct
- confirm PyPI metadata, wheel files, and supported Python versions look correct
- confirm COPT-enabled wheels still require only the vendor COPT runtime and do
  not require a separate manual shim build by end users
- confirm targeted GitHub Release artifacts are clearly labeled by CPU baseline
- confirm the docs still make only true claims
