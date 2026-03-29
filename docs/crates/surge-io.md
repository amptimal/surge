# surge-io

`surge-io` is the interchange layer around `surge_network::Network`. It is the
main crate for loading, saving, parsing, and serializing network case data in
Rust.

## Root Surface

Prefer the crate-root API:

```rust
use surge_io::{Format, dumps, load, loads, save};
```

The root surface is small:

- `load(path)` for extension-driven filesystem input
- `save(&network, path)` for extension-driven filesystem output
- `loads(text, Format)` for in-memory text parsing
- `dumps(&network, Format)` for in-memory text serialization
- `Format`, `LoadError`, and `SaveError` for shared root types

The root API covers common one-file workflows. Format-specific behavior lives
under namespaced modules.

For native text interchange, `surge_io::json` reads and writes the versioned
`surge-json` document envelope. The current schema version is `0.1.0`.
For native binary interchange, `surge_io::bin` reads and writes the versioned
`surge-bin` container. It keeps the same logical `Network` model while using a
packed sectioned layout for high-volume entities and typed extension sections
for the rest of the model.

Canonical profile CSV readers also live here:

- `surge_io::profiles::read_load_profiles_csv`
- `surge_io::profiles::read_renewable_profiles_csv`

OpenDSS load-shape types live under `surge_io::dss::LoadShape`.

## Namespace Layout

Use the per-format modules when you need explicit control:

```rust
surge_io::matpower::{load, save, loads, dumps}
surge_io::psse::raw::{load, save, loads, dumps, Version}
surge_io::psse::rawx::{load, loads}
surge_io::psse::dyr::{load, save, loads, dumps}
surge_io::psse::dyd::loads
surge_io::psse::sequence::{apply, apply_text}
surge_io::cgmes::{load, load_all, loads, save, to_profiles, Version}
surge_io::bin::{load, save, loads, dumps}
surge_io::json::{load, save, loads, dumps}
surge_io::xiidm::{load, save, loads, dumps}
surge_io::ucte::{load, save, loads, dumps}
surge_io::dss::{load, save, loads, dumps}
surge_io::epc::{load, save, loads, dumps}
surge_io::ieee_cdf::{load, loads}
surge_io::geo::apply_bus_coordinates
surge_io::export::{write_network_csv, write_solution_snapshot}
```

Design rules:

- `load` / `save` mean filesystem I/O
- `loads` / `dumps` mean in-memory text I/O
- `apply` means mutate an existing network with sidecar data
- CGMES export is explicit and directory-based only

## Common Examples

### Generic file I/O

```rust
use std::path::Path;
use surge_io::{load, save};

let network = load(Path::new("examples/cases/ieee118/case118.surge.json.zst"))?;
save(&network, Path::new("case118.surge.json.zst"))?;
surge_io::bin::save(&network, Path::new("case118.surge.bin"))?;
```

### Explicit format I/O

```rust
use surge_io::matpower;
use surge_io::psse::raw;

let net = matpower::load("case118.m")?;
let raw_text = raw::dumps(&net, raw::Version::V35)?;
let round_trip = raw::loads(&raw_text)?;
assert_eq!(round_trip.n_buses(), net.n_buses());
```

### Sequence data

```rust
use surge_io::psse::{raw, sequence};

let mut net = raw::load("case.raw")?;
let stats = sequence::apply(&mut net, "case.seq")?;
println!("machines_updated={}", stats.machines_updated);
```

### Dynamic data

```rust
use surge_io::psse::dyr;

let dyn_model = dyr::load("case.dyr")?;
let text = dyr::dumps(&dyn_model)?;
let dyn_model_2 = dyr::loads(&text)?;
```

### CGMES

```rust
use surge_io::cgmes;

let net = cgmes::load("cgmes_bundle.zip")?;
cgmes::save(&net, "out/cgmes_v3", cgmes::Version::V3_0)?;
let profiles = cgmes::to_profiles(&net, cgmes::Version::V2_4_15)?;
println!("EQ profile bytes={}", profiles.eq.len());
```

`surge_io::save(...)` does not infer CGMES output from `.xml` or `.cim`. Use `surge_io::cgmes::save(...)` explicitly so the directory semantics are obvious.

## Supported Formats

| Format | Read | Write | Canonical module |
|---|---|---|---|
| MATPOWER `.m` | yes | yes | `surge_io::matpower` |
| PSS/E RAW `.raw` | yes | yes | `surge_io::psse::raw` |
| PSS/E RAWX `.rawx` | yes | no | `surge_io::psse::rawx` |
| PSS/E DYR `.dyr` | yes | yes | `surge_io::psse::dyr` |
| PSS/E DYD text | yes | no | `surge_io::psse::dyd` |
| PSS/E SEQ `.seq` | sidecar | no | `surge_io::psse::sequence` |
| IEEE CDF `.cdf` | yes | no | `surge_io::ieee_cdf` |
| CGMES directory / `.xml` / `.cim` / `.zip` | yes | yes | `surge_io::cgmes` |
| XIIDM `.xiidm` / `.iidm` | yes | yes | `surge_io::xiidm` |
| UCTE `.uct` / `.ucte` | yes | yes | `surge_io::ucte` |
| OpenDSS `.dss` | yes | yes | `surge_io::dss` |
| GE PSLF `.epc` | yes | yes | `surge_io::epc` |
| Surge JSON `.surge.json` / `.surge.json.zst` | yes | yes | `surge_io::json` |
| Surge BIN `.surge.bin` | yes | yes | `surge_io::bin` |
| Network CSV export | no | yes | `surge_io::export` |

## Shared Root Format Detection

`surge_io::load(...)` recognizes:

- `.m`
- `.raw`
- `.rawx`
- `.cdf`
- `.xiidm` / `.iidm`
- `.uct` / `.ucte`
- `.xml` / `.cim`
- `.zip`
- `.epc`
- `.dss`
- `.surge.json`
- `.surge.json.zst`
- `.surge.bin`
- directories, which are treated as CGMES bundles

`surge_io::save(...)` recognizes:

- `.m`
- `.raw`
- `.xiidm` / `.iidm`
- `.uct` / `.ucte`
- `.dss`
- `.epc`
- `.surge.json`
- `.surge.json.zst`
- `.surge.bin`

For in-memory parsing, use `loads(text, Format::...)`. `Format` is intentionally limited to text-like formats handled by the root API.

## Related Helpers

Not everything in `surge-io` is a case-file parser. These helpers are also part of the crate:

- `surge_io::geo::apply_bus_coordinates(...)` enriches buses from a CSV sidecar
- `surge_io::export::write_network_csv(...)` writes a tabular network snapshot
- `surge_io::export::write_solution_snapshot(...)` writes solved-state CSV output
- `surge_io::cgmes::ext::*` contains specialized CGMES extension parsing and profile helpers
- `surge_io::scl::*`, `surge_io::pscad::*`, `surge_io::comtrade::*`, and related modules remain available for specialized workflows

Those advanced modules stay namespaced on purpose. The crate root is reserved for the canonical load/save/loads/dumps path.

## Release Gates

The minimum verification bar for this crate is:

```bash
cargo check -p surge-io
cargo test -p surge-io --no-run
```

When the Python bindings change with `surge-io`, also re-check:

```bash
cargo check -p surge-py
```

## Take

If you are writing application code:

- start with `surge_io::load` / `surge_io::save`
- drop into `surge_io::<format>::...` when you need explicit control
- treat `surge_io::cgmes::save` and `surge_io::psse::sequence::apply` as explicit special cases, not generic save/load variants
