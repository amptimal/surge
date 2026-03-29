# surge-hvdc

`surge-hvdc` provides the HVDC power-flow layer in the Surge workspace. It
covers point-to-point LCC/VSC links, explicit DC-grid solves, and the stable
root entry points that route between the supported solver families.

## Stable Root API

The root surface is intentionally small:

- `solve_hvdc` for HVDC assets embedded in a `surge_network::Network`
- `solve_hvdc_links` for explicitly provided point-to-point `HvdcLink` values
- `HvdcOptions` and `HvdcMethod` for solver selection
- `HvdcSolution`, `HvdcStationSolution`, `HvdcDcBusSolution`, and
  `HvdcTechnology` for the canonical result contract
- root re-exports of the public HVDC model types such as `HvdcLink`, `LccLink`,
  `VscLink`, `LccControlMode`, `VscControlMode`, and `TapControl`

## Method Routing

`solve_hvdc` chooses a method based on topology when
`options.method == HvdcMethod::Auto`:

- `Sequential` for point-to-point HVDC links only
- `BlockCoupled` for one explicit VSC-only `dc_grid`
- `Hybrid` for one explicit `dc_grid` that includes at least one LCC converter

The root API rejects mixed representations. A single solve must use either
point-to-point links or one explicit DC-grid topology, not both.

## Namespaces

- `model` contains the public HVDC domain types
- `advanced` exposes lower-level stable solver namespaces for callers that need
  direct access to sequential, block-coupled, or hybrid solver internals
- `experimental` contains solver paths that are not part of the stable root API
- `interop` contains workspace-facing helpers for applying DC-grid injections to
  `surge-network` models

## Example

```rust,no_run
use surge_hvdc::{solve_hvdc, HvdcOptions};
use surge_network::Network;

let net = Network::new("example");
let options = HvdcOptions::default();
let _solution = solve_hvdc(&net, &options);
```

## Notes

- `HvdcMethod::BlockCoupled` requires an explicit DC grid with VSC converters.
- `HvdcMethod::Hybrid` requires an explicit DC grid with at least one LCC
  converter.
- The experimental simultaneous AC/DC solver is not re-exported on the root
  API by design.
