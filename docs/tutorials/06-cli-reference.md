# Tutorial 06: CLI Reference

`surge-solve` is the command-line entry point for the current Surge workspace.
The volatile method and option surface is generated from the live CLI help so
it does not need to be hand-maintained.

## Generated Contract

- [Generated CLI surface](../generated/cli-surface.md)

That generated page is the release-facing contract for methods, options, and
accepted values. If it and a built binary disagree, regenerate the doc from the
current binary and treat the binary as authoritative.

## Build And Inspect

```bash
cargo build --release --bin surge-solve
python scripts/render_public_surface.py --cli-bin target/release/surge-solve
```

## Examples

Assume:

```bash
CASE=examples/cases/ieee118/case118.surge.json.zst
```

### Parse And Inspect

```bash
./target/release/surge-solve "$CASE" --parse-only
./target/release/surge-solve "$CASE" --convert /tmp/case118.m --export-format matpower
./target/release/surge-solve "$CASE" --method acpf --export /tmp/case118-result.json
```

### Power Flow

```bash
./target/release/surge-solve "$CASE" --method acpf
./target/release/surge-solve "$CASE" --method acpf-warm --tolerance 1e-10
./target/release/surge-solve "$CASE" --method fdpf
./target/release/surge-solve "$CASE" --method dcpf --output json
```

### Contingency

```bash
./target/release/surge-solve "$CASE" --method contingency --screening lodf
./target/release/surge-solve "$CASE" --method n-2 --screening lodf
./target/release/surge-solve "$CASE" --method contingency --voltage-stress-mode exact_l_index --output json
```

### Optimization And Dispatch

```bash
./target/release/surge-solve "$CASE" --method dc-opf --solver highs
./target/release/surge-solve "$CASE" --method ac-opf --solver ipopt
./target/release/surge-solve "$CASE" --method scopf --scopf-formulation dc
```

### Transfer

```bash
./target/release/surge-solve "$CASE" --method injection-capability
./target/release/surge-solve "$CASE" --method nerc-atc --source-buses 8 --sink-buses 1
```

## Notes

- Regenerate [../generated/cli-surface.md](../generated/cli-surface.md) whenever
  the public CLI contract changes.
- Keep examples here narrow and user-facing; keep the volatile option surface in
  the generated doc.
- `--convert` writes a network file in the requested output format. `--export`
  writes a solved-state artifact and only accepts `.json` or `.json.zst`.
- `--export-format` only applies to `--convert`.
