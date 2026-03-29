# Support And Compatibility

This page summarizes the supported build and release surface for this
repository.

## Public Surfaces

- Rust workspace crates
- `surge-solve` CLI
- `surge` Python package built from `src/surge-py`

## Supported Versions

### Rust

- MSRV: Rust 1.87+

### Python

- Supported versions: 3.12 through 3.14

The Python package metadata and wheel workflows are expected to match that
range exactly.

## Build Requirements

### Required For All Source Builds

- C/C++ compiler (`gcc`, `clang`, or MSVC)

### Optional: HiGHS (LP Solver)

HiGHS is the default LP backend for `surge-opf`. It is loaded at runtime via
`libloading`, the same as Ipopt, Gurobi, COPT, and CPLEX.

Install with your system package manager:

- macOS: `brew install highs`
- Ubuntu / Debian: `sudo apt install libhighs-dev`

If HiGHS is installed to a custom location, set `HIGHS_LIB_DIR` to the
directory containing `libhighs.{so,dylib}`.

### Required For AC Workflows

- SuiteSparse / KLU development libraries

### Required For AC-OPF

- Ipopt runtime library (`libipopt.so` on Linux, `libipopt.dylib` on macOS)

### Optional Runtime Solvers

- Gurobi
- COPT
- CPLEX

Commercial solvers and HiGHS are discovered at runtime via `libloading`.

## Source Build Support

Supported source-build paths:

- Rust builds from the workspace root with Cargo
- Python builds through `maturin develop --release` from `src/surge-py/`

See [contributing/setup.md](contributing/setup.md) for platform-specific setup
commands.

## Platform Notes

### Tested Build Platforms

| Platform | Architecture | Toolchain | Notes |
|---|---|---|---|
| Ubuntu / Debian | x86_64, aarch64 | gcc, system packages | CI default |
| macOS | aarch64 (Apple Silicon) | Xcode CLT + Homebrew | `IPOPT_LIB_DIR=/opt/homebrew/lib` for runtime Ipopt |
| macOS | x86_64 (Intel) | Xcode CLT + Homebrew | `IPOPT_LIB_DIR=/usr/local/lib` for runtime Ipopt |
| Windows | x86_64 | MSVC + vcpkg | KLU via vcpkg; no Ipopt |

### Public Wheel Targets

- Linux x86_64
- Linux aarch64
- macOS aarch64
- Windows x86_64

These define the generic PyPI wheel matrix. Additional targeted
`x86-64-v4` wheels, when built, are GitHub Release artifacts only.

## Packaging Notes

- The Rust crates intended for crates.io publication are:
  `surge-network`, `surge-solution`, `surge-sparse`, `surge-io`,
  `surge-topology`, `surge-dc`, `surge-ac`, `surge-hvdc`,
  `surge-contingency`, `surge-transfer`, and `surge-opf`.
- The interface/packaging crates `surge-bindings` and `surge-py` are workspace
  members but are not published to crates.io.
- Generic Python wheels are published to PyPI as part of the coordinated
  release process.
- Targeted `x86-64-v4` wheels, when built, are GitHub Release artifacts only.
- Native text case files use `surge-json` at schema version `0.1.0`.
- Native binary case files use `surge-bin` at schema version `0.1.0`.
