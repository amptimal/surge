# surge-sparse

Sparse matrix utilities for the Surge workspace.

This crate packages sparse data structures and factorization helpers used by
the AC, DC, HVDC, and OPF layers, including CSC utilities and KLU-oriented
support code for solver-facing sparse workflows.

## Native Dependency

`surge-sparse` links against SuiteSparse KLU.

- Ubuntu / Debian: `sudo apt install libsuitesparse-dev`
- Fedora: `sudo dnf install suitesparse-devel`
- macOS (Homebrew): `brew install suite-sparse`
- Windows (vcpkg): `vcpkg install suitesparse-klu:x64-windows-static`

If your libraries live outside the platform default search paths, set
`SUITESPARSE_LIB_DIR` before building.
