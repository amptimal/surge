# Contributing To Surge

Surge accepts contributions through pull requests. Treat `main` as PR-only.

Start with:

- [docs/contributing/setup.md](docs/contributing/setup.md) for environment setup
- [docs/support-compatibility.md](docs/support-compatibility.md) if your change
  affects supported versions, platforms, or packaging assumptions

Open an issue first when possible for large architecture or interface changes.

## Ground Rules

- Keep public docs aligned with the current source surface.
- Prefer narrowing a claim over guessing.
- If a public contract changes, update the user-facing docs in the same PR.
- Do not add references to crates, workflows, files, or compatibility promises
  that do not exist in this repository.

## Build And Test

Run the checks that match your change before opening a PR.

### Rust workspace baseline

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --exclude surge-py
```

Use `--exclude surge-py` for baseline workspace testing. The Python crate is
built and tested through maturin and Python tooling rather than through
`cargo test -p surge-py`.

### CLI build

```bash
cargo build --release --bin surge-solve
./target/release/surge-solve --help
```

### Python package build and tests

```bash
cd src/surge-py
maturin develop --release
cd ../..
pytest src/surge-py/tests/ -x -v --tb=short
```

Targeted Rust runs that are often useful:

```bash
cargo test -p surge-ac
cargo test -p surge-dc
cargo test -p surge-contingency
cargo test -p surge-opf
cargo test -p surge-transfer
```

Some extended tests depend on case libraries from the separate `surge-bench`
repository and skip gracefully when that data is absent.

## Documentation Expectations

Update the implementation and the release-facing docs together.

- CLI changes:
  `src/surge-bindings/src/main.rs`,
  `docs/tutorials/06-cli-reference.md`,
  `docs/crates/surge-bindings.md`
- Python changes:
  `src/surge-py/python/surge/__init__.pyi`,
  `docs/tutorials/05-python-api.md`,
  `docs/notebooks/README.md`,
  `docs/crates/surge-py.md`
- Build, support, or packaging changes:
  `README.md`,
  `docs/support-compatibility.md`,
  `docs/contributing/setup.md`,
  `RELEASING.md`

If you change a public crate surface, update its crate README or matching page
under `docs/crates/` in the same PR.

## Pull Requests

Each PR should make it easy to answer:

- what changed
- why it changed
- how it was tested
- which public docs or user-facing claims changed with it

If there are known follow-ups or remaining gaps, call them out plainly.

## Licensing

Surge is source-available under PolyForm Noncommercial 1.0.0. Commercial use
requires a separate license from Amptimal.

If contributor paperwork is required for a change, maintainers will provide it
during review. Do not add links to in-repo CLA files that do not exist.

## Security

Do not file public issues for security vulnerabilities. Follow
[SECURITY.md](SECURITY.md).
