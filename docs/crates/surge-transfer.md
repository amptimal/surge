# surge-transfer

`surge-transfer` is the transfer-capability crate built on the DC and AC
solver layers. It provides NERC-style Available Transfer Capability (ATC),
Available Flowgate Capability (AFC), AC-aware transfer limits, simultaneous
multi-interface transfer studies, and injection-capability screening.

## Methodology

### Transfer Paths

A `TransferPath` defines a directional transfer between source and sink buses.
Power is injected equally across the source buses and withdrawn equally across
the sink buses. The transfer direction matters: ATC from A to B is not
necessarily equal to ATC from B to A.

```python
path = surge.transfer.TransferPath("north_to_south", source_buses=[1, 2], sink_buses=[8, 9])
```

### Available Transfer Capability (ATC)

ATC follows the NERC MOD-029 / MOD-030 methodology:

```text
ATC = TTC - TRM - CBM - ETC
```

| Term | Meaning |
|---|---|
| **TTC** (Total Transfer Capability) | Maximum transfer limited by thermal constraints |
| **TRM** (Transmission Reliability Margin) | Margin for uncertainties in system conditions |
| **CBM** (Capacity Benefit Margin) | Margin reserved for generation reliability |
| **ETC** (Existing Transmission Commitments) | Already-committed transfers |

Surge computes TTC as the maximum MW transfer along a path before any
monitored branch reaches its thermal rating, considering both base-case (N-0)
and post-contingency (N-1) conditions. The limiting constraint is identified
using Power Transfer Distribution Factors (PTDFs) and Line Outage Distribution
Factors (LODFs).

TRM, CBM, and ETC are user-supplied margin inputs via `AtcOptions.margins`.
ATC is clamped to zero (no negative ATC).

The result includes:

- `atc_mw` — the ATC value
- `ttc_mw` — the raw thermal headroom before margin deductions
- `binding_branch` — the branch index that limits the transfer
- `binding_contingency` — the contingency that produces the binding constraint
  (if N-1 is enabled)
- `transfer_ptdf` — per-monitored-branch sensitivity to the transfer injection
- `reactive_margin_warning` — flags paths where generators approach reactive
  limits, suggesting the DC-based TTC may be optimistic

### AC ATC

`compute_ac_atc` extends the DC-based ATC with voltage screening. After the
DC-PTDF-based transfer limit is found, an AC power flow confirms that the
operating point is feasible in the full nonlinear model. Voltage violations
and reactive limits may reduce the effective ATC below the DC estimate.

### Available Flowgate Capability (AFC)

AFC answers: how much additional transfer can flow through a specific
flowgate before it reaches its rating?

A `Flowgate` is defined by:

- A monitored branch
- An optional contingency branch (for N-1 flowgates)
- A MW rating

AFC is computed per-flowgate using the transfer PTDF and LODF. The result
identifies the binding flowgate and contingency.

### Multi-Transfer

`compute_multi_transfer` evaluates simultaneous transfer on multiple paths
with optional weighting. It finds the maximum scaling factor such that all
monitored branches remain within limits when all transfers are applied
simultaneously.

### Injection Capability

Injection-capability screening computes the maximum MW injection at each bus
before any monitored branch reaches its thermal rating. This supports FERC
Order 2023 interconnection study requirements and generation siting analysis.

### Matrix-Level Sensitivities

The `matrices` submodule exposes:

- **GSF** (Generation Shift Factors) — sensitivity of each branch flow to a
  1 MW injection at each bus.
- **BLDF** (Bus Load Distribution Factors) — sensitivity of each branch flow
  to a 1 MW load at each bus, relative to a distributed reference.

These are the building blocks for ATC, AFC, and injection-capability
calculations.

## Root API

Entrypoints:

- `compute_nerc_atc` — NERC-methodology DC-based ATC
- `compute_ac_atc` — AC-confirmed ATC with voltage screening
- `compute_afc` — Available Flowgate Capability
- `compute_multi_transfer` — simultaneous multi-interface transfer
- `TransferStudy` — reusable prepared study for repeated transfer work

Request and result types:

- `TransferPath`, `AtcOptions`, `AtcMargins`
- `NercAtcRequest`, `NercAtcResult`
- `AcAtcRequest`, `AcAtcResult`
- `AfcRequest`, `AfcResult`, `Flowgate`
- `MultiTransferRequest`, `MultiTransferResult`

## AtcOptions Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `monitored_branches` | list | `None` | Branch indices to monitor (None = all rated branches) |
| `contingency_branches` | list | `None` | Outage branches for N-1 (None = skip N-1) |
| `margins` | `AtcMargins` | zeros | TRM, CBM, ETC deductions in MW |

## NercAtcResult Reference

| Field | Type | Description |
|---|---|---|
| `atc_mw` | float | Available Transfer Capability (MW), clamped to 0 |
| `ttc_mw` | float | Total Transfer Capability before margin deductions |
| `trm_mw` | float | Transmission Reliability Margin applied |
| `cbm_mw` | float | Capacity Benefit Margin applied |
| `etc_mw` | float | Existing Transmission Commitments applied |
| `binding_branch` | optional | Index of the branch that limits transfer |
| `binding_contingency` | optional | Index of the contingency causing the binding constraint |
| `transfer_ptdf` | array | Per-monitored-branch sensitivity to the transfer |
| `reactive_margin_warning` | bool | True if generators near reactive limits |

## Prepared Study

`TransferStudy` prepares the DC factorization and sensitivity matrices once,
then reuses them across multiple ATC, AFC, and multi-transfer queries on the
same network:

```python
import surge

net = surge.load("case118.surge.json.zst")
study = surge.transfer.prepare_transfer_study(net)

path_a = surge.transfer.TransferPath("a_to_b", [1], [10])
path_b = surge.transfer.TransferPath("c_to_d", [5], [20])

atc_a = study.compute_nerc_atc(path_a, surge.transfer.AtcOptions())
atc_b = study.compute_nerc_atc(path_b, surge.transfer.AtcOptions())
```

## When To Use Each Method

| Need | Method |
|---|---|
| Standard NERC ATC screening | `compute_nerc_atc` |
| ATC with voltage feasibility check | `compute_ac_atc` |
| Flowgate-specific headroom | `compute_afc` |
| Multi-path simultaneous transfer | `compute_multi_transfer` |
| Generation siting / interconnection | injection capability |
| Repeated studies on one network | `TransferStudy` |

## Related Docs

- [Data Model And Conventions](../data-model.md) for rating conventions
- [Method Fidelity](../method-fidelity.md)
- [References](../references.md)
- [surge-dc](surge-dc.md) for underlying PTDF/LODF computations
- [Tutorial 04](../tutorials/04-transfer-capability.md)
