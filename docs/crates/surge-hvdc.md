# surge-hvdc

`surge-hvdc` is the workspace HVDC crate. It provides the stable root contract
for HVDC power flow, lower-level namespaces for advanced solver access, and a
small bridge layer used by other `surge-*` crates.

## Stable Root Surface

For normal client code, use the crate root:

```rust
use surge_hvdc::{
    solve_hvdc, solve_hvdc_links, HvdcError, HvdcMethod, HvdcOptions, HvdcSolution,
};
```

The stable root exports:

- `solve_hvdc`
- `solve_hvdc_links`
- `HvdcError`
- `HvdcOptions`
- `HvdcMethod`
- `HvdcSolution`
- `HvdcStationSolution`
- `HvdcDcBusSolution`
- `HvdcLccDetail`
- `HvdcTechnology`
- public model types re-exported from `model`, including `HvdcLink`, `LccLink`,
  `VscLink`, `LccControlMode`, `VscControlMode`, `TapControl`,
  `VscStationState`, `CommutationCheck`, and `check_commutation_failure`

The root does not export a generic `solve` alias, and it does not expose the
experimental simultaneous solver.

## Namespaces

### `model`

The `model` namespace contains the public HVDC domain types and converter
physics abstractions. The most common model types are also re-exported at the
crate root.

### `advanced`

`advanced` exposes lower-level but stable solver entry points:

- `advanced::sequential::solve_hvdc_links`
- `advanced::block_coupled::solve`
- `advanced::hybrid::solve`

It also re-exports supporting solver-side types such as `DcNetwork`,
`BlockCoupledAcDcSolverOptions`, `VscStation`, `HybridMtdcNetwork`,
`LccConverter`, and the related result structs.

### `experimental`

`experimental::simultaneous` contains the experimental simultaneous AC/DC
Newton solve:

- `experimental::simultaneous::solve`
- `experimental::simultaneous::SimultaneousAcDcSolverOptions`

Treat that namespace as non-stable.

### `interop`

`interop` is a workspace-facing bridge layer rather than the main client API.
Its currently exposed helpers are:

- `links_from_network`
- `dc_grid_injections`
- `dc_grid_injections_from_voltages`
- `apply_dc_grid_injections`

It also exposes raw PSS/E-style conversion helpers under `interop::psse`.

## Method Selection

`solve_hvdc(&Network, &HvdcOptions)` resolves `HvdcMethod::Auto` this way:

- `Sequential` when the network uses point-to-point HVDC links only
- `BlockCoupled` when the network has one explicit `dc_grid` and the converters
  are VSC-only
- `Hybrid` when the network has one explicit `dc_grid` and at least one LCC
  converter

The root API rejects unsupported topology combinations:

- mixed point-to-point links plus explicit DC-grid topology
- more than one explicit DC grid in a single solve
- `Sequential` requested for explicit DC-grid topology
- `BlockCoupled` requested for explicit LCC-containing DC grids
- `Hybrid` requested for pure-VSC explicit DC grids

## Result Contract

`HvdcSolution` is the stable aggregate result returned by both
`solve_hvdc` and `solve_hvdc_links`.

- `stations` contains one `HvdcStationSolution` per converter terminal
- `dc_buses` is populated for explicit DC-grid solves
- `total_converter_loss_mw`, `total_dc_network_loss_mw`, and `total_loss_mw`
  summarize losses
- `iterations` reports the outer or hybrid solve count
- `converged` reports final convergence status
- `method` records the method actually used

`HvdcStationSolution` uses the stable sign convention:

- `p_ac_mw < 0`: power drawn from AC
- `p_ac_mw > 0`: power injected into AC
- `p_dc_mw > 0`: power injected into the DC network
- `p_dc_mw < 0`: power drawn from the DC network

## Examples

### Solve HVDC Embedded In A Network

```rust,no_run
use surge_hvdc::{solve_hvdc, HvdcOptions};
use surge_network::Network;

let network = Network::new("example");
let options = HvdcOptions::default();
let _solution = solve_hvdc(&network, &options)?;
# Ok::<(), surge_hvdc::HvdcError>(())
```

### Solve Explicit Point-To-Point Links

```rust,no_run
use surge_hvdc::{solve_hvdc_links, HvdcLink, HvdcOptions, VscLink};
use surge_network::Network;

let network = Network::new("example");
let links = vec![HvdcLink::Vsc(VscLink::new(1, 2, 200.0))];
let options = HvdcOptions::default();
let _solution = solve_hvdc_links(&network, &links, &options)?;
# Ok::<(), surge_hvdc::HvdcError>(())
```

## Validation Pointers

Primary HVDC validation coverage in this repository lives under:

- `src/surge-hvdc/tests/test_cigre.rs`
- `src/surge-hvdc/tests/test_pglib_validation.rs`
- `src/surge-hvdc/tests/test_coverage_expansion.rs`
- `src/surge-hvdc/tests/test_hvdc.rs`

See also [../method-fidelity.md](../method-fidelity.md).
