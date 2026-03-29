# surge-bindings - CLI Interface Crate

`surge-bindings` builds the `surge-solve` CLI and a small supporting `rlib`.

## Authoritative Contract

The authoritative CLI contract is:

- the `clap` definition in `src/surge-bindings/src/main.rs`
- the live output of `surge-solve --help`
- the generated CLI reference in
  [../generated/cli-surface.md](../generated/cli-surface.md)

## Build

```bash
cargo build --release --bin surge-solve
./target/release/surge-solve --help
```

## Notes

- Keep this page short and defer volatile option details to the generated CLI
  surface.
- When prose and live CLI help disagree, the live help wins.
