# surge-network

`surge-network` is the shared power-system domain model crate. It owns the
`Network` type and all equipment records used across importers, topology
processing, steady-state solvers, optimization, and interface layers.

This crate contains model data and validation logic only. It does not run
power flow, OPF, topology reduction, or any other solver algorithm.

## Network Structure

The `Network` struct is the central data container. Its key components:

### Core Electrical Equipment

| Collection | Type | Description |
|---|---|---|
| `buses` | `Vec<Bus>` | All buses with voltage, type, and area/zone |
| `branches` | `Vec<Branch>` | Lines, transformers, series capacitors, zero-impedance ties |
| `generators` | `Vec<Generator>` | Generation units with cost curves and operating limits |
| `loads` | `Vec<Load>` | Demand loads with ZIP voltage dependence |
| `fixed_shunts` | `Vec<FixedShunt>` | Capacitor banks, reactors, harmonic filters |
| `facts_devices` | `Vec<FactsDevice>` | SVC, STATCOM, TCSC devices |

### System Parameters

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | string | — | Case name |
| `base_mva` | float | `100.0` | System MVA base for per-unit calculations |
| `freq_hz` | float | `60.0` | System frequency in Hz |

### Controls And Constraints

| Field | Description |
|---|---|
| `controls` | Switched shunts, OLTC tap changers, phase-angle regulators |
| `area_schedules` | Area interchange MW targets and regulating generators |
| `interfaces` | Transmission interface definitions (monitored branch groups) |
| `flowgates` | Monitored flowgate definitions for contingency and transfer studies |

### HVDC

The `hvdc` field contains:

- `links` — point-to-point HVDC links (LCC and VSC)
- `dc_grids` — explicit multi-terminal DC grid topologies with DC buses,
  DC branches, and converter stations

### Topology

When a network originates from CGMES, XIIDM, or another node-breaker source,
the optional `topology` field contains:

- `substations` — physical substations
- `voltage_levels` — voltage levels within substations
- `bays` — switchgear bays
- `connectivity_nodes` — node-breaker connectivity nodes
- `switches` — circuit breakers and disconnectors
- `mapping` — the current bus-branch-to-node-breaker mapping

See [surge-topology](surge-topology.md) for topology rebuild workflows.

### Market And Dispatch Data

The `market_data` field contains:

- Dispatchable loads
- Pumped hydro units
- Combined-cycle plant configurations
- Reserve zones and offers
- Outage schedules
- Emission rates and policy data

These are used by dispatch and market simulation workflows on feature branches
and by OPF formulations that model storage and dispatchable demand.

## Entity Types

Detailed field-level schema for the core entity types is documented in
[Data Model And Conventions](../data-model.md). Key highlights:

### Bus

Buses are identified by `number` (u32). Bus `type` determines the power flow
treatment: `PQ` (load), `PV` (voltage-regulated generator), `Slack`
(reference), or `Isolated`. Each bus carries initial voltage magnitude and
angle, nominal kV, area/zone assignment, and OPF voltage bounds.

### Branch

Branches represent transmission lines, transformers, series capacitors, and
zero-impedance ties. Impedance (`r`, `x`, `b`) is in per-unit on the system
base. Transformers add `tap` (off-nominal turns ratio) and `phase_shift_rad`.
Three thermal rating tiers (A/B/C) support base-case and contingency studies.

Optional sub-structs provide:
- `line_data` — physical line parameters (length, conductor, temperature)
- `transformer_data` — winding connections, ratings, grounding impedance
- `zero_seq` — zero-sequence impedance for fault analysis
- `opf_control` — tap/phase optimization bounds for AC-OPF

### Generator

Generators carry active/reactive dispatch, operating limits, voltage setpoint,
cost curve, and type classification. Optional sub-structs add:
- `storage` — battery or pumped-hydro parameters (capacity, SoC, efficiency)
- `commitment` — unit commitment parameters (min up/down time, forbidden zones)
- `ramping` — ramp rate curves for dispatch and regulation
- `inverter` — IBR-specific parameters (available power, grid-forming capability)
- `reactive_capability` — P-Q capability curve data points
- `fuel` — fuel type, heat rate, emission rates
- `market` — energy and reserve offers

### Load

Loads carry active and reactive demand with full ZIP voltage dependence model.
The three fractions (impedance, current, power) for each of P and Q must sum
to 1.0. Default is pure constant-power. Load classification, connection type,
and shedding priority are available for advanced studies.

## Validation

`Network` performs structural validation:

- Bus numbers must be unique and positive.
- Branch endpoints must reference existing buses.
- Generator and load buses must exist in the bus list.
- Per-unit impedances and ratings must be finite.

Validation errors are reported as `NetworkError`.

## Convenience Accessors

In Python, the `Network` object exposes:

- `n_buses`, `n_branches`, `n_generators`, `n_loads` — element counts
- `buses`, `branches`, `generators`, `loads` — element lists
- `topology` — node-breaker topology (if present)

## Related Docs

- [Data Model And Conventions](../data-model.md) for per-unit conventions and sign rules
- [surge-io](surge-io.md) for loading and saving networks
- [surge-topology](surge-topology.md) for node-breaker topology
- [Glossary](../glossary.md) for power-systems terminology
