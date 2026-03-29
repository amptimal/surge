# Glossary

Power-systems and optimization terminology used in Surge documentation.

## Analysis Methods

| Term | Expansion | Description |
|---|---|---|
| **ACPF** | AC Power Flow | Nonlinear steady-state solution of the full AC power flow equations |
| **DCPF** | DC Power Flow | Linear approximation assuming flat voltages, small angles, lossless branches |
| **FDPF** | Fast Decoupled Power Flow | Approximate AC method exploiting P-theta / Q-V decoupling |
| **NR** | Newton-Raphson | Iterative nonlinear solver with quadratic convergence; the standard method for ACPF |
| **OPF** | Optimal Power Flow | Economic dispatch subject to network constraints |
| **DC-OPF** | DC Optimal Power Flow | OPF using DC power flow constraints (LP or QP) |
| **AC-OPF** | AC Optimal Power Flow | OPF using full AC power flow constraints (NLP) |
| **SCOPF** | Security-Constrained OPF | OPF that also satisfies post-contingency constraints |
| **SCED** | Security-Constrained Economic Dispatch | Real-time market dispatch engine (typically DC-OPF based) |
| **ORPD** | Optimal Reactive Power Dispatch | Optimization of reactive resources to minimize losses or voltage deviation |
| **OTS** | Optimal Transmission Switching | Optimization of branch switching status to reduce cost or relieve congestion |
| **CPF** | Continuation Power Flow | Traces the power flow solution as load/generation scales toward a bifurcation |

## Sensitivity Factors

| Term | Expansion | Description |
|---|---|---|
| **PTDF** | Power Transfer Distribution Factor | Sensitivity of branch flow to a 1 MW injection at a bus (relative to slack) |
| **LODF** | Line Outage Distribution Factor | Sensitivity of branch flow to the outage of another branch |
| **OTDF** | Outage Transfer Distribution Factor | Combined PTDF under a contingency: OTDF = PTDF + LODF × PTDF |
| **GSF** | Generation Shift Factor | Sensitivity of branch flow to a 1 MW generation injection at a bus |
| **BLDF** | Bus Load Distribution Factor | Sensitivity of branch flow to a 1 MW load change at a bus |
| **DFAX** | Distribution Factor | General term for linear sensitivity factors used in transfer studies |

## Transfer Capability

| Term | Expansion | Description |
|---|---|---|
| **ATC** | Available Transfer Capability | Maximum additional transfer between areas (NERC definition) |
| **TTC** | Total Transfer Capability | Maximum transfer limited by thermal/voltage/stability constraints |
| **TRM** | Transmission Reliability Margin | Margin for system condition uncertainties |
| **CBM** | Capacity Benefit Margin | Margin reserved for generation reliability |
| **ETC** | Existing Transmission Commitments | Already-committed firm and non-firm transfers |
| **AFC** | Available Flowgate Capability | Remaining transfer capability through a specific flowgate |

## Market And Pricing

| Term | Expansion | Description |
|---|---|---|
| **LMP** | Locational Marginal Price | Cost of serving the next MW of load at a bus ($/MWh) |
| **AGC** | Automatic Generation Control | Real-time generation adjustment to maintain frequency and interchange |
| **APF** | Area Participation Factor | Generator share of area interchange regulation |

## Equipment

| Term | Expansion | Description |
|---|---|---|
| **OLTC** | On-Load Tap Changer | Transformer tap that adjusts under load to regulate voltage |
| **PAR** | Phase-Angle Regulator | Phase-shifting transformer that controls MW flow |
| **SVC** | Static VAR Compensator | Thyristor-controlled shunt reactive device |
| **STATCOM** | Static Synchronous Compensator | Voltage-source converter providing dynamic reactive support |
| **TCSC** | Thyristor-Controlled Series Capacitor | Series compensation device for power flow control |
| **FACTS** | Flexible AC Transmission Systems | Family of power-electronic devices for transmission control |
| **HVDC** | High-Voltage Direct Current | DC transmission technology for long-distance or asynchronous connections |
| **LCC** | Line-Commutated Converter | Thyristor-based HVDC converter (classic HVDC) |
| **VSC** | Voltage Source Converter | IGBT-based HVDC converter (modern HVDC) |
| **MTDC** | Multi-Terminal DC | DC grid with more than two converter terminals |
| **BESS** | Battery Energy Storage System | Grid-scale battery for energy storage and dispatch |
| **IBR** | Inverter-Based Resource | Generation connected through a power electronic inverter (wind, solar, BESS) |

## Network Modeling

| Term | Expansion | Description |
|---|---|---|
| **Bus** | — | A node in the network model representing a point of constant voltage |
| **PQ bus** | — | Load bus: P and Q specified, V and theta solved |
| **PV bus** | — | Generator bus: P and |V| specified, Q and theta solved |
| **Slack bus** | — | Reference bus: |V| and theta specified, P and Q solved |
| **Branch** | — | A connection between two buses (line, transformer, series device) |
| **Pi model** | — | Standard equivalent circuit for a transmission line or transformer |
| **ZIP model** | — | Load model: weighted sum of constant-impedance (Z), constant-current (I), constant-power (P) |
| **Per-unit** | — | Normalized quantity system based on chosen base values (MVA, kV) |
| **Island** | — | An electrically connected subnetwork within a larger system |

## Topology

| Term | Expansion | Description |
|---|---|---|
| **Bus-branch** | — | Simplified network model used by power flow solvers |
| **Node-breaker** | — | Physical topology model with switches, connectivity nodes, substations |
| **CGMES** | Common Grid Model Exchange Standard | IEC 61970-based data exchange format for European TSOs |
| **CIM** | Common Information Model | IEC standard data model for power systems |
| **XIIDM** | — | XML-based network exchange format (PowSyBl ecosystem) |

## Contingency Analysis

| Term | Expansion | Description |
|---|---|---|
| **N-1** | — | Analysis of single-element outages |
| **N-2** | — | Analysis of simultaneous two-element outages |
| **N-1-1** | — | Sequential two-element outage (outage, adjust, second outage) |
| **TPL** | Transmission Planning | NERC reliability standard family (TPL-001 through TPL-007) |
| **RAS** | Remedial Action Scheme | Automated corrective action triggered by specific system conditions |
| **SCRD** | Security-Constrained Redispatch | Corrective generation redispatch to relieve post-contingency violations |

## File Formats

| Term | Description |
|---|---|
| **MATPOWER** | MATLAB-based power flow data format (.m files) |
| **PSS/E RAW** | Siemens PTI power flow data format (.raw files) |
| **PSS/E RAWX** | JSON-based PSS/E format (.rawx files) |
| **PSS/E DYR** | PSS/E dynamic model data format (.dyr files) |
| **IEEE CDF** | IEEE Common Data Format for power flow (.cdf files) |
| **UCTE-DEF** | UCTE Data Exchange Format for European networks (.uct files) |
| **OpenDSS** | EPRI distribution system simulator format (.dss files) |
| **GE EPC** | GE PSLF power flow data format (.epc files) |
| **Surge JSON** | Surge native text format (.surge.json, .surge.json.zst) |
| **Surge BIN** | Surge native binary format (.surge.bin) |

## Standards And Organizations

| Term | Expansion |
|---|---|
| **NERC** | North American Electric Reliability Corporation |
| **FERC** | Federal Energy Regulatory Commission |
| **RTO** | Regional Transmission Organization |
| **ISO** | Independent System Operator |
| **MOD-029** | NERC standard for ATC calculation methodology |
| **MOD-030** | NERC standard for flowgate methodology |
