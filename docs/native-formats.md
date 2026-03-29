# Native Formats

Surge ships three native network file variants with one shared schema story.

## Canonical Extensions

- `.surge.json` — canonical inspectable `surge-json` document
- `.surge.json.zst` — zstd-compressed `surge-json` document
- `.surge.bin` — native `surge-bin` binary container

## When To Use Which

- Use `.surge.json` when you want a human-inspectable canonical file.
- Use `.surge.json.zst` when you want the same logical document with better disk
  efficiency.
- Use `.surge.bin` when you want the fastest full-model Surge-native load and
  save path.

## Contract

- `surge-json` schema version: `0.1.0`
- `surge-bin` schema version: `0.1.0`

Schema versions are tracked separately from the workspace package version.

## APIs

Rust:

```rust
use surge_io::{load, save};

let net = load("examples/cases/ieee118/case118.surge.json.zst")?;
save(&net, "case118.surge.json")?;
surge_io::bin::save(&net, "case118.surge.bin")?;
```

Python:

```python
import surge

net = surge.load("examples/cases/ieee118/case118.surge.json.zst")
surge.save(net, "case118.surge.json")
surge.io.bin.save(net, "case118.surge.bin")
```

CLI:

```bash
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst \
  --convert /tmp/case118.surge.json
./target/release/surge-solve examples/cases/ieee118/case118.surge.json.zst \
  --convert /tmp/case118.surge.bin
```

## Notes

- Top-level `load` and `save` infer the native format from the extension.
- `surge.io.json` is the explicit text-format module.
- `surge.io.bin` is the explicit binary-format module.
- `surge-bin` is a sectioned binary container with packed core entity sections
  plus typed extension sections for the rest of the Surge model.
- `python3 scripts/benchmark_native_formats.py` runs the packaged native-format
  cycle benchmark against the shipped example bundles.
- `python3 scripts/benchmark_native_formats.py --validate-solutions` solves the
  MATPOWER baseline and validates that the native formats match across ACPF,
  DCPF, DCOPF, and ACOPF.
- Pretty JSON is opt-in only. Default JSON output is compact.
