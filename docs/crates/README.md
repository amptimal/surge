# Crate Documentation

Per-crate documentation for the Surge workspace.

## Foundation

| Crate | Description |
|---|---|
| [surge-network](surge-network.md) | Shared power-system domain model |
| [surge-solution](surge-solution.md) | Shared result contracts and replay helpers |
| [surge-sparse](surge-sparse.md) | Sparse matrix types and KLU wrappers |
| [surge-io](surge-io.md) | File I/O for all supported formats |
| [surge-topology](surge-topology.md) | Node-breaker topology rebuild |

## Solvers

| Crate | Description |
|---|---|
| [surge-dc](surge-dc.md) | DC power flow, PTDF, LODF, and prepared DC studies |
| [surge-ac](surge-ac.md) | AC Newton-Raphson and fast-decoupled power flow |
| [surge-hvdc](surge-hvdc.md) | HVDC power flow for LCC/VSC links and DC grids |
| [surge-contingency](surge-contingency.md) | N-1, N-2 contingency analysis with screening |
| [surge-opf](surge-opf.md) | DC-OPF, AC-OPF, SCOPF, OTS, and ORPD |
| [surge-transfer](surge-transfer.md) | ATC, AFC, and transfer capability studies |

## Markets

| Crate | Description |
|---|---|
| [surge-dispatch](surge-dispatch.md) | Unified SCED and SCUC kernel — DC/AC, period-by-period or time-coupled, with reserves, security screening, and SCED-AC Benders |
| [surge-market](surge-market.md) | Canonical market-formulation layer — reserve catalogues, multi-stage workflows, AC SCED setup, retry/refinement runtime, GO C3 adapter |

## Interfaces

| Crate | Description |
|---|---|
| [surge-bindings](surge-bindings.md) | `surge-solve` CLI |
| [surge-py](surge-py.md) | Python bindings and typed package layer |
