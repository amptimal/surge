# surge-solution

`surge-solution` holds the shared solved-state contracts used by the solver
crates.

## Public Surface

- `PfSolution`
- `SolveStatus`
- `OpfSolution`, `AcOpfSolution`, `DcOpfSolution`, `OpfType`
- `ParSetpoint`, `ParResult`
- `AreaInterchangeResult`, `AreaInterchangeEntry`, `AreaDispatchMethod`
- replay helpers:
  `apply_bus_voltages`, `apply_dispatch_mw`, `apply_opf_dispatch`

## What This Crate Is For

- sharing one power-flow result contract across AC, DC, HVDC, and follow-on
  workflows
- sharing OPF result contracts across `surge-opf` and interface layers
- applying solved voltages or dispatch back onto a mutable
  `surge_network::Network`

## Notes

- This crate does not expose solver entrypoints.
- Use it when you need stable result types or replay helpers across multiple
  solver crates.
