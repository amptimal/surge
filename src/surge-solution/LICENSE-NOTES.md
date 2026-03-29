# License Notes

Surge is licensed under the PolyForm Noncommercial License 1.0.0. See
[LICENSE](LICENSE) for the license text and
[COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md) for commercial licensing.

This page is a practical summary of third-party software used by this
repository. It is informational only. If you distribute Surge or build it into
another product, review the licenses of the components you ship.

## What This Covers

- Rust crates pulled through `Cargo.lock`
- vendored build dependencies used by this repository
- system libraries and optional solver runtimes used by source builds or
  released artifacts

## Rust Crates

Surge depends on third-party Rust crates through Cargo. The current dependency
set can change over time, so this file does not hard-code a crate count.

To inspect the current dependency licenses locally:

```bash
cargo license
```

`Cargo.lock` remains the definitive record of the exact versions used for a
given build.

## Third-Party Components Used By Surge

### HiGHS

- Role: LP and MIP solver backend for supported optimization workflows
- Upstream project: <https://github.com/ERGO-Code/HiGHS>
- Upstream license: MIT

HiGHS is an optional runtime dependency discovered via `libloading`. It is not
vendored or compiled from source. Install it as a system package
(`brew install highs` on macOS, `apt install libhighs-dev` on Debian/Ubuntu).

### SuiteSparse / KLU

- Role: sparse linear algebra used by AC and related workflows
- Typical source-build package: `libsuitesparse-dev` on Debian/Ubuntu
- Upstream project: <https://github.com/DrTimothyAldenDavis/SuiteSparse>
- Upstream license family: SuiteSparse projects include components under their
  own upstream terms

SuiteSparse and KLU are part of the documented native dependency set for source
builds. Release artifacts may package the shared libraries needed for the
artifact being built.

### Ipopt

- Role: AC-OPF and related nonlinear optimization workflows
- Typical source-build package: `coinor-libipopt-dev` on Debian/Ubuntu
- Upstream project: <https://github.com/coin-or/Ipopt>
- Upstream license: EPL-2.0

Ipopt is an optional dependency. Users who do not need AC-OPF do not need to
install it.

### Clarabel

- Role: conic solver backend for AC-OPF workflows
- Upstream project: <https://github.com/oxfordcontrol/Clarabel.rs>
- Upstream license: Apache-2.0

Clarabel is a Cargo dependency compiled into the workspace.

### Commercial Solver Integrations

Surge includes integrations for separately licensed commercial solvers where
supported by the current codebase.

These include:

- Gurobi
- COPT
- CPLEX

Using those integrations requires you to obtain the relevant software and
comply with the vendor's license terms. Surge does not grant rights to those
products.

## Source Builds And Released Artifacts

The exact third-party components present in a build depend on how Surge is
built and distributed.

- Source builds depend on the native libraries described in the current build
  documentation.
- Python wheels may include redistributed shared libraries needed by that wheel.
- Optional commercial solver integrations depend on software installed outside
  this repository.

For the supported build paths, see
[docs/support-compatibility.md](docs/support-compatibility.md) and
[docs/contributing/setup.md](docs/contributing/setup.md).

## Questions

- Licensing for Surge itself: `licensing@amptimal.com`
- Technical packaging questions: open an issue in this repository
