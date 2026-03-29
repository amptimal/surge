# Data Model And Conventions

This page documents the per-unit system, sign conventions, and core entity
schema used throughout Surge. These conventions apply uniformly to the Rust
crates, the Python package, the CLI, and the native file formats.

## Per-Unit System

Surge stores all electrical quantities in the per-unit system on a common
system base.

| Quantity | Internal unit | Base |
|---|---|---|
| Voltage magnitude | per-unit | `base_kv` of the local bus |
| Voltage angle | radians | absolute |
| Active power | MW | — |
| Reactive power | MVAr | — |
| Apparent power | MVA | — |
| Branch impedance (`r`, `x`) | per-unit | system `base_mva` and `from_bus` `base_kv` |
| Branch charging (`b`) | per-unit | system `base_mva` and `from_bus` `base_kv` |
| Tap ratio | per-unit | off-nominal turns ratio (1.0 = no transformation) |
| Phase shift | radians | — |
| Shunt conductance / susceptance | MW / MVAr at V = 1.0 p.u. | — |
| Branch thermal ratings | MVA | — |

The default system base is **100 MVA** (`Network.base_mva = 100.0`). All
per-unit impedances, admittances, and injections are on this base unless stated
otherwise.

### Converting Physical To Per-Unit

Branch impedance from ohms to per-unit:

```text
Z_base = base_kv^2 / base_mva

r_pu = r_ohm / Z_base
x_pu = x_ohm / Z_base
b_pu = b_siemens * Z_base
```

The Python helper `surge.units.ohm_to_pu(ohm, base_kv, base_mva)` performs
this conversion.

### Transformer Per-Unit Convention

Transformer `r` and `x` are stored in per-unit on the system base and the
**from-bus (winding-1) `base_kv`**, matching MATPOWER and PSS/E convention. The
`tap` field is the off-nominal turns ratio:

- `tap = 1.0` means the transformer operates at nominal ratio.
- `tap > 1.0` means the from-side voltage is boosted relative to the to-side.
- `tap < 1.0` means the from-side voltage is bucked.

For phase-shifting transformers, `phase_shift_rad` is the additional angle
shift introduced by the transformer, in radians. Positive shift advances the
from-side angle relative to the to-side.

## Sign Conventions

| Quantity | Sign convention |
|---|---|
| Generator active power (`p`) | Positive = injecting into the network |
| Generator reactive power (`q`) | Positive = injecting vars into the network |
| Load active power (`active_power_demand_mw`) | Positive = consuming from the network |
| Load reactive power (`reactive_power_demand_mvar`) | Positive = consuming vars from the network |
| Branch flow (`branch_p_from_mw`) | Positive = power flowing from the from-bus into the branch |
| Shunt susceptance (`b_mvar`) | Positive = capacitive (generating vars) |
| Shunt conductance (`g_mw`) | Positive = power consumption |
| HVDC station `p_ac_mw` | Positive = injecting into the AC network; negative = drawing from AC |
| HVDC station `p_dc_mw` | Positive = injecting into the DC network; negative = drawing from DC |

## System Frequency

`Network.freq_hz` defaults to **60 Hz**. Individual buses may override this
with `bus.freq_hz` for mixed-frequency systems. Solvers use the network-level
default unless a bus-level override is present.

## Core Entity Schema

### Network

The top-level container. Key fields:

| Field | Type | Description |
|---|---|---|
| `name` | string | Case name |
| `base_mva` | float | System MVA base (default 100.0) |
| `freq_hz` | float | System frequency in Hz (default 60.0) |
| `buses` | list | All buses |
| `branches` | list | All branches (lines and transformers) |
| `generators` | list | All generators |
| `loads` | list | All loads |
| `fixed_shunts` | list | Fixed shunt devices |
| `facts_devices` | list | FACTS devices (SVC, STATCOM, TCSC) |
| `hvdc` | object | HVDC links and DC grids |
| `topology` | optional | Node-breaker topology (from CGMES/XIIDM) |
| `controls` | object | Switched shunts, OLTCs, PARs |
| `area_schedules` | list | Area interchange schedules |
| `interfaces` | list | Transmission interfaces |
| `flowgates` | list | Monitored flowgates |

### Bus

| Field | Type | Unit | Description |
|---|---|---|---|
| `number` | u32 | — | Unique bus identifier |
| `name` | string | — | Human-readable name |
| `bus_type` | enum | — | `PQ`, `PV`, `Slack`, or `Isolated` |
| `base_kv` | float | kV | Nominal voltage |
| `voltage_magnitude_pu` | float | p.u. | Initial or solved voltage magnitude |
| `voltage_angle_rad` | float | rad | Initial or solved voltage angle |
| `voltage_min_pu` | float | p.u. | OPF lower voltage bound |
| `voltage_max_pu` | float | p.u. | OPF upper voltage bound |
| `shunt_conductance_mw` | float | MW | Fixed shunt G at V = 1.0 p.u. |
| `shunt_susceptance_mvar` | float | MVAr | Fixed shunt B at V = 1.0 p.u. |
| `area` | u32 | — | Area number |
| `zone` | u32 | — | Zone number |

**Bus types:**

- **PQ** — Load bus. Active and reactive power are specified; voltage is solved.
- **PV** — Generator bus. Active power and voltage magnitude are specified;
  reactive power is solved within `[qmin, qmax]`. If the reactive limit is
  hit, the bus switches to PQ.
- **Slack** — Reference bus. Voltage magnitude and angle are specified; active
  and reactive power are solved to close the system power balance.
- **Isolated** — Disconnected bus. Excluded from the solve.

### Branch

| Field | Type | Unit | Description |
|---|---|---|---|
| `from_bus` | u32 | — | From-bus number |
| `to_bus` | u32 | — | To-bus number |
| `circuit` | string | — | Circuit identifier for parallel branches |
| `r` | float | p.u. | Series resistance |
| `x` | float | p.u. | Series reactance |
| `b` | float | p.u. | Total line charging susceptance |
| `tap` | float | p.u. | Off-nominal turns ratio (1.0 = line or nominal transformer) |
| `phase_shift_rad` | float | rad | Phase-shifter angle |
| `rating_a_mva` | float | MVA | Long-term (continuous) thermal rating |
| `rating_b_mva` | float | MVA | Short-term (emergency) thermal rating |
| `rating_c_mva` | float | MVA | Ultimate emergency thermal rating |
| `in_service` | bool | — | Whether the branch is in service |
| `branch_type` | enum | — | `Line`, `Transformer`, `Transformer3W`, `SeriesCapacitor`, `ZeroImpedanceTie` |
| `g_mag` | float | p.u. | Magnetizing conductance (transformers) |
| `b_mag` | float | p.u. | Magnetizing susceptance (transformers) |

The branch admittance model follows the standard pi-equivalent circuit. For
transmission lines, `b` is the total line charging susceptance (split equally
between the two ends). For transformers, `tap` and `phase_shift_rad` define
the complex turns ratio `t = tap * exp(j * phase_shift_rad)`.

### Generator

| Field | Type | Unit | Description |
|---|---|---|---|
| `id` | string | — | Stable generator identifier |
| `bus` | u32 | — | Bus number |
| `p` | float | MW | Active power output |
| `q` | float | MVAr | Reactive power output |
| `pmax` | float | MW | Maximum active power |
| `pmin` | float | MW | Minimum active power (negative for storage charging) |
| `qmax` | float | MVAr | Maximum reactive power |
| `qmin` | float | MVAr | Minimum reactive power |
| `voltage_setpoint_pu` | float | p.u. | Voltage regulation target |
| `machine_base_mva` | float | MVA | Machine base rating |
| `in_service` | bool | — | Whether the generator is online |
| `gen_type` | enum | — | `Synchronous`, `Wind`, `Solar`, `InverterOther` |
| `cost` | optional | — | Cost curve for OPF dispatch |

Generators with `cost` data participate in economic dispatch. The cost curve
can be polynomial (up to quadratic: `c0 + c1*P + c2*P^2` in $/hr) or
piecewise-linear (breakpoint pairs of MW and $/hr).

### Load

| Field | Type | Unit | Description |
|---|---|---|---|
| `bus` | u32 | — | Bus number |
| `id` | string | — | Load identifier |
| `active_power_demand_mw` | float | MW | Active demand |
| `reactive_power_demand_mvar` | float | MVAr | Reactive demand |
| `in_service` | bool | — | Whether the load is connected |
| `zip_p_impedance_frac` | float | — | Constant-impedance P fraction |
| `zip_p_current_frac` | float | — | Constant-current P fraction |
| `zip_p_power_frac` | float | — | Constant-power P fraction (default 1.0) |
| `zip_q_impedance_frac` | float | — | Constant-impedance Q fraction |
| `zip_q_current_frac` | float | — | Constant-current Q fraction |
| `zip_q_power_frac` | float | — | Constant-power Q fraction (default 1.0) |

The ZIP model expresses load as a weighted sum of constant-impedance (Z),
constant-current (I), and constant-power (P) components. The three fractions
for each of P and Q must sum to 1.0. The default is pure constant-power
(`zip_p_power_frac = 1.0`, `zip_q_power_frac = 1.0`), which is the standard
assumption for transmission-level steady-state studies.

### Fixed Shunt

| Field | Type | Unit | Description |
|---|---|---|---|
| `bus` | u32 | — | Bus number |
| `id` | string | — | Shunt identifier |
| `g_mw` | float | MW | Conductance at V = 1.0 p.u. |
| `b_mvar` | float | MVAr | Susceptance at V = 1.0 p.u. (positive = capacitive) |
| `in_service` | bool | — | Whether the shunt is connected |

## Thermal Ratings

Branches carry three thermal ratings used by different study types:

| Rating | Field | Typical use |
|---|---|---|
| Rate A | `rating_a_mva` | Base-case continuous limit; default for OPF and contingency |
| Rate B | `rating_b_mva` | Short-term emergency; post-contingency limit in some RTO practices |
| Rate C | `rating_c_mva` | Ultimate emergency; extreme contingency scenarios |

A rating of 0.0 means the branch is unconstrained for that tier. When a
post-contingency study selects Rate B or Rate C and the value is zero, Surge
falls back to Rate A.

## Multi-Island Networks

Surge automatically detects electrically disconnected islands. Each island
gets its own slack bus and is solved independently. The `island_id` field on
each bus records which island it belongs to after topology processing.

## Related Pages

- [Glossary](glossary.md) for power-systems terminology
- [References](references.md) for governing equations and literature
- [Native Formats](native-formats.md) for file format details
- [Crate: surge-network](crates/surge-network.md) for the Rust API surface
