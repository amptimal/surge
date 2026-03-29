// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Contingency definitions for N-1/N-2/extreme-event security analysis.
//!
//! A contingency is an outage of one or more network elements (branches,
//! generators) used in security-constrained power flow and OPF studies.
//! This module lives in surge-network so that both surge-opf (which needs
//! `Contingency` as a constraint input) and surge-contingency (which runs
//! the parallel AC solve) can share the type without a circular dependency.
//!
//! NERC TPL-001-5.1 extreme event categories (P4/P5/P6) are supported via
//! [`TplCategory`] classification and dedicated contingency generators.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

use crate::network::{
    BusType, LccHvdcControlMode, Network, NodeBreakerTopology, SwitchType, VscHvdcControlMode,
};

/// A network modification applied simultaneously with this contingency.
///
/// Represents PSS/E .con `SET`/`CHANGE`/`ALTER`/`MODIFY`/`INCREASE`/`DECREASE`
/// commands within a `CONTINGENCY` block — network state changes that occur
/// at the same instant as the element outages.
///
/// Applied to the per-contingency network clone before the post-contingency
/// power flow is solved. The base-case network is never mutated.
///
/// **JSON/Python representation**: internally tagged with `"type"` field, e.g.
/// `{"type": "BranchTap", "from_bus": 1, "to_bus": 2, "circuit": 1, "tap": 1.05}`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContingencyModification {
    /// Bring a branch back in-service (close a previously open branch).
    ///
    /// PSS/E .con: `SET STATUS CLOSE BRANCH FROM BUS <i> TO BUS <j> [CKT <c>]`
    BranchClose {
        from_bus: u32,
        to_bus: u32,
        circuit: String,
    },
    /// Set transformer off-nominal tap ratio.
    ///
    /// PSS/E .con: `SET TAP OF BRANCH FROM BUS <i> TO BUS <j> [CKT <c>] TO <val>`
    BranchTap {
        from_bus: u32,
        to_bus: u32,
        circuit: String,
        tap: f64,
    },
    /// Set bus active and reactive load to absolute values (MW, MVAr).
    ///
    /// PSS/E .con: `SET PLOAD AT BUS <n> TO <p>` / `CHANGE LOAD AT BUS <n> TO <p> <q>`
    /// Fails if the target bus does not exist.
    LoadSet { bus: u32, p_mw: f64, q_mvar: f64 },
    /// Adjust bus load by a relative change (delta MW, delta MVAr).
    ///
    /// PSS/E .con: `INCREASE PLOAD AT BUS <n> BY <delta>` / `DECREASE QLOAD ...`
    /// Fails if the target bus does not exist.
    LoadAdjust {
        bus: u32,
        delta_p_mw: f64,
        delta_q_mvar: f64,
    },
    /// Set generator real power output directly.
    ///
    /// PSS/E .con: `SET PGEN OF UNIT <id> AT BUS <n> TO <val>`
    GenOutputSet {
        bus: u32,
        machine_id: String,
        p_mw: f64,
    },
    /// Set generator real power limit (pmax and/or pmin).
    ///
    /// PSS/E .con: `SET PMAX OF UNIT <id> AT BUS <n> TO <val>` / `SET PMIN ...`
    GenLimitSet {
        bus: u32,
        machine_id: String,
        pmax_mw: Option<f64>,
        pmin_mw: Option<f64>,
    },
    /// Adjust bus fixed shunt susceptance by a delta (p.u.).
    ///
    /// The network stores fixed bus shunt susceptance in MVAr, so this delta
    /// is converted using `network.base_mva` when applied.
    ///
    /// PSS/E .con: `CHANGE SHUNT AT BUS <n> BY <delta>` / `INCREASE SHUNT ...`
    ShuntAdjust { bus: u32, delta_b_pu: f64 },
    /// Change bus type (1=PQ, 2=PV, 3=Slack).
    ///
    /// PSS/E .con: `CHANGE BUS TYPE BUS <n> TO TYPE <t>`
    BusTypeChange { bus: u32, bus_type: u32 },
    /// Set area interchange scheduled export.
    ///
    /// PSS/E .con: `CHANGE AREA INTERCHANGE <n> TO <val>`
    AreaScheduleSet { area: u32, p_mw: f64 },
    /// Block (trip) a two-terminal LCC-HVDC line by name.
    ///
    /// Sets `LccHvdcLink.mode = LccHvdcControlMode::Blocked`, causing zero P/Q injection
    /// at both rectifier and inverter buses.
    ///
    /// PSS/E .con: `BLOCK TWOTERMDC '<name>'`
    DcLineBlock { name: String },
    /// Block (trip) a VSC-HVDC link by name.
    ///
    /// Sets `VscHvdcLink.mode = VscHvdcControlMode::Blocked`, causing zero P/Q injection
    /// at both converter buses.
    ///
    /// PSS/E .con: `BLOCK VSCDC '<name>'`
    VscDcLineBlock { name: String },
    /// Remove a switched shunt device from service at a given bus.
    ///
    /// Removes the switched shunt's contribution from the bus fixed shunt
    /// susceptance (`bus.shunt_susceptance_mvar`).  If `switched_shunts_opf` data is available
    /// for the bus, the device's current `b_init_pu` is subtracted (in MVAr
    /// after conversion by `network.base_mva`) and its OPF range is zeroed.
    /// If no switched shunt device is present at the bus, the modification
    /// fails so the contingency definition stays honest.
    ///
    /// PSS/E .con: `REMOVE SWSHUNT [<id>] FROM BUS <n>`
    SwitchedShuntRemove { bus: u32 },
    /// Trip a single converter in an explicit DC grid.
    ///
    /// Sets the matching canonical explicit-DC converter out of service.
    /// Fails if the converter does not exist.
    ///
    /// Used to model loss of a DC-grid converter terminal during contingency analysis
    /// without requiring surge-network to depend on surge-hvdc.
    DcGridConverterTrip { converter_id: String },
}

/// Error raised when a contingency modification cannot be applied exactly.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContingencyModificationError {
    /// The requested bus does not exist.
    #[error("{operation}: bus {bus} not found in network")]
    MissingBus { operation: &'static str, bus: u32 },
    /// The requested branch does not exist.
    #[error("{operation}: branch {from_bus}-{to_bus} ckt {circuit} not found in network")]
    MissingBranch {
        operation: &'static str,
        from_bus: u32,
        to_bus: u32,
        circuit: String,
    },
    /// The requested generator does not exist.
    #[error("{operation}: generator {machine_id} at bus {bus} not found in network")]
    MissingGenerator {
        operation: &'static str,
        bus: u32,
        machine_id: String,
    },
    /// The requested area schedule does not exist.
    #[error("{operation}: area {area} not found in network")]
    MissingAreaSchedule { operation: &'static str, area: u32 },
    /// The requested HVDC link does not exist.
    #[error("{operation}: HVDC link `{name}` not found in network")]
    MissingHvdcLink {
        operation: &'static str,
        name: String,
    },
    /// The requested DC-grid converter does not exist.
    #[error("{operation}: DC-grid converter `{converter_id}` not found")]
    MissingDcGridConverter {
        operation: &'static str,
        converter_id: String,
    },
    /// The requested switched shunt does not exist at the target bus.
    #[error("{operation}: no switched shunt exists at bus {bus}")]
    MissingSwitchedShunt { operation: &'static str, bus: u32 },
    /// The requested bus type code is not recognized.
    #[error("{operation}: invalid bus type code {bus_type} for bus {bus}")]
    InvalidBusType {
        operation: &'static str,
        bus: u32,
        bus_type: u32,
    },
}

/// Apply a list of contingency modifications to a cloned network in-place.
///
/// Called after element outages are applied, before the post-contingency power
/// flow is solved. The base-case network is never mutated; only the clone is changed.
///
/// Unknown bus numbers, branch pairs, or generator IDs are rejected with an
/// explicit error so the contingency cannot silently mutate into a different case.
pub fn apply_contingency_modifications(
    network: &mut Network,
    modifications: &[ContingencyModification],
) -> Result<(), ContingencyModificationError> {
    if modifications.is_empty() {
        return Ok(());
    }
    let bus_map = network.bus_index_map();
    for modification in modifications {
        match modification {
            ContingencyModification::BranchClose {
                from_bus,
                to_bus,
                circuit,
            } => {
                let mut matched = false;
                for br in &mut network.branches {
                    if branch_matches(
                        br.from_bus,
                        br.to_bus,
                        &br.circuit,
                        *from_bus,
                        *to_bus,
                        circuit,
                    ) {
                        br.in_service = true;
                        matched = true;
                    }
                }
                if !matched {
                    return Err(ContingencyModificationError::MissingBranch {
                        operation: "BranchClose",
                        from_bus: *from_bus,
                        to_bus: *to_bus,
                        circuit: circuit.clone(),
                    });
                }
            }
            ContingencyModification::BranchTap {
                from_bus,
                to_bus,
                circuit,
                tap,
            } => {
                let mut matched = false;
                for br in &mut network.branches {
                    if branch_matches(
                        br.from_bus,
                        br.to_bus,
                        &br.circuit,
                        *from_bus,
                        *to_bus,
                        circuit,
                    ) {
                        br.tap = *tap;
                        matched = true;
                    }
                }
                if !matched {
                    return Err(ContingencyModificationError::MissingBranch {
                        operation: "BranchTap",
                        from_bus: *from_bus,
                        to_bus: *to_bus,
                        circuit: circuit.clone(),
                    });
                }
            }
            ContingencyModification::LoadSet { bus, p_mw, q_mvar } => {
                if !bus_map.contains_key(bus) {
                    return Err(ContingencyModificationError::MissingBus {
                        operation: "LoadSet",
                        bus: *bus,
                    });
                }
                // Set total load at this bus to the specified values.
                // First zero out all existing loads at this bus, then set the first one
                // (or create one if none exists) to the target values.
                let loads_at_bus: Vec<usize> = network
                    .loads
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.bus == *bus)
                    .map(|(i, _)| i)
                    .collect();
                if loads_at_bus.is_empty() {
                    // No load at this bus — create one.
                    network
                        .loads
                        .push(crate::network::Load::new(*bus, *p_mw, *q_mvar));
                } else {
                    // Set the first load to the target, zero the rest.
                    for (rank, &li) in loads_at_bus.iter().enumerate() {
                        if rank == 0 {
                            network.loads[li].active_power_demand_mw = *p_mw;
                            network.loads[li].reactive_power_demand_mvar = *q_mvar;
                            network.loads[li].in_service = true;
                        } else {
                            network.loads[li].active_power_demand_mw = 0.0;
                            network.loads[li].reactive_power_demand_mvar = 0.0;
                        }
                    }
                }
            }
            ContingencyModification::LoadAdjust {
                bus,
                delta_p_mw,
                delta_q_mvar,
            } => {
                if !bus_map.contains_key(bus) {
                    return Err(ContingencyModificationError::MissingBus {
                        operation: "LoadAdjust",
                        bus: *bus,
                    });
                }
                // Adjust load at this bus proportionally across all loads.
                let loads_at_bus: Vec<usize> = network
                    .loads
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.bus == *bus && l.in_service)
                    .map(|(i, _)| i)
                    .collect();
                if loads_at_bus.is_empty() {
                    // No load at this bus — create one with the delta as the value.
                    network
                        .loads
                        .push(crate::network::Load::new(*bus, *delta_p_mw, *delta_q_mvar));
                } else if loads_at_bus.len() == 1 {
                    // Single load — apply delta directly.
                    let li = loads_at_bus[0];
                    network.loads[li].active_power_demand_mw += delta_p_mw;
                    network.loads[li].reactive_power_demand_mvar += delta_q_mvar;
                } else {
                    // Multiple loads — distribute delta proportionally by MW.
                    let total_p: f64 = loads_at_bus
                        .iter()
                        .map(|&i| network.loads[i].active_power_demand_mw.abs())
                        .sum();
                    let total_q: f64 = loads_at_bus
                        .iter()
                        .map(|&i| network.loads[i].reactive_power_demand_mvar.abs())
                        .sum();
                    for &li in &loads_at_bus {
                        let p_frac = if total_p > 1e-12 {
                            network.loads[li].active_power_demand_mw.abs() / total_p
                        } else {
                            1.0 / loads_at_bus.len() as f64
                        };
                        let q_frac = if total_q > 1e-12 {
                            network.loads[li].reactive_power_demand_mvar.abs() / total_q
                        } else {
                            1.0 / loads_at_bus.len() as f64
                        };
                        network.loads[li].active_power_demand_mw += delta_p_mw * p_frac;
                        network.loads[li].reactive_power_demand_mvar += delta_q_mvar * q_frac;
                    }
                }
            }
            ContingencyModification::GenOutputSet {
                bus,
                machine_id,
                p_mw,
            } => {
                let mut matched = false;
                for g in &mut network.generators {
                    if g.bus == *bus
                        && g.machine_id.as_deref().unwrap_or("1") == machine_id.as_str()
                    {
                        // g.p is in MW; bus_p_injection_pu() divides by base_mva.
                        g.p = *p_mw;
                        matched = true;
                    }
                }
                if !matched {
                    return Err(ContingencyModificationError::MissingGenerator {
                        operation: "GenOutputSet",
                        bus: *bus,
                        machine_id: machine_id.clone(),
                    });
                }
            }
            ContingencyModification::GenLimitSet {
                bus,
                machine_id,
                pmax_mw,
                pmin_mw,
            } => {
                let mut matched = false;
                for g in &mut network.generators {
                    if g.bus == *bus
                        && g.machine_id.as_deref().unwrap_or("1") == machine_id.as_str()
                    {
                        if let Some(pmax) = pmax_mw {
                            g.pmax = *pmax;
                        }
                        if let Some(pmin) = pmin_mw {
                            g.pmin = *pmin;
                        }
                        matched = true;
                    }
                }
                if !matched {
                    return Err(ContingencyModificationError::MissingGenerator {
                        operation: "GenLimitSet",
                        bus: *bus,
                        machine_id: machine_id.clone(),
                    });
                }
            }
            ContingencyModification::ShuntAdjust { bus, delta_b_pu } => {
                let Some(&idx) = bus_map.get(bus) else {
                    return Err(ContingencyModificationError::MissingBus {
                        operation: "ShuntAdjust",
                        bus: *bus,
                    });
                };
                network.buses[idx].shunt_susceptance_mvar += delta_b_pu * network.base_mva;
            }
            ContingencyModification::BusTypeChange { bus, bus_type } => {
                let Some(&idx) = bus_map.get(bus) else {
                    return Err(ContingencyModificationError::MissingBus {
                        operation: "BusTypeChange",
                        bus: *bus,
                    });
                };
                let new_type = match bus_type {
                    1 => BusType::PQ,
                    2 => BusType::PV,
                    3 => BusType::Slack,
                    _ => {
                        return Err(ContingencyModificationError::InvalidBusType {
                            operation: "BusTypeChange",
                            bus: *bus,
                            bus_type: *bus_type,
                        });
                    }
                };
                network.buses[idx].bus_type = new_type;
            }
            ContingencyModification::AreaScheduleSet { area, p_mw } => {
                let mut matched = false;
                for ai in &mut network.area_schedules {
                    if ai.number == *area {
                        ai.p_desired_mw = *p_mw;
                        matched = true;
                    }
                }
                if !matched {
                    return Err(ContingencyModificationError::MissingAreaSchedule {
                        operation: "AreaScheduleSet",
                        area: *area,
                    });
                }
            }
            ContingencyModification::DcLineBlock { name } => {
                let mut found = false;
                for link in &mut network.hvdc.links {
                    if let Some(dc) = link.as_lcc_mut()
                        && dc.name == *name
                    {
                        dc.mode = LccHvdcControlMode::Blocked;
                        found = true;
                    }
                }
                if !found {
                    return Err(ContingencyModificationError::MissingHvdcLink {
                        operation: "DcLineBlock",
                        name: name.clone(),
                    });
                }
            }
            ContingencyModification::VscDcLineBlock { name } => {
                let mut found = false;
                for link in &mut network.hvdc.links {
                    if let Some(vsc) = link.as_vsc_mut()
                        && vsc.name == *name
                    {
                        vsc.mode = VscHvdcControlMode::Blocked;
                        found = true;
                    }
                }
                if !found {
                    return Err(ContingencyModificationError::MissingHvdcLink {
                        operation: "VscDcLineBlock",
                        name: name.clone(),
                    });
                }
            }
            ContingencyModification::SwitchedShuntRemove { bus } => {
                // Find bus index.
                let Some(&bus_idx) = bus_map.get(bus) else {
                    return Err(ContingencyModificationError::MissingBus {
                        operation: "SwitchedShuntRemove",
                        bus: *bus,
                    });
                };

                // Primary path: discrete SwitchedShunt model (MODSW != 0 shunts).
                // The shunt's contribution was NOT baked into bus.shunt_susceptance_mvar during parsing,
                // so we zero out the device's step range — no bus.shunt_susceptance_mvar adjustment needed.
                let mut removed = false;
                for ss in &mut network.controls.switched_shunts {
                    if ss.bus == *bus {
                        ss.n_steps_cap = 0;
                        ss.n_steps_react = 0;
                        ss.n_active_steps = 0;
                        removed = true;
                        info!("SwitchedShuntRemove: zeroed discrete shunt at bus {}", bus);
                    }
                }

                // Fallback path: OPF continuous shunt model (switched_shunts_opf).
                // These shunts' BINIT *was* baked into bus.shunt_susceptance_mvar (pre-new-model files),
                // so subtract b_init_pu from bus.shunt_susceptance_mvar and zero out the OPF variable.
                if !removed {
                    for ss in &mut network.controls.switched_shunts_opf {
                        if ss.bus == *bus {
                            network.buses[bus_idx].shunt_susceptance_mvar -=
                                ss.b_init_pu * network.base_mva;
                            ss.b_min_pu = 0.0;
                            ss.b_max_pu = 0.0;
                            ss.b_init_pu = 0.0;
                            removed = true;
                            break;
                        }
                    }
                }

                // No switched shunt device was found at the bus.
                if !removed {
                    return Err(ContingencyModificationError::MissingSwitchedShunt {
                        operation: "SwitchedShuntRemove",
                        bus: *bus,
                    });
                }
            }
            ContingencyModification::DcGridConverterTrip { converter_id } => {
                let mut found = false;
                for grid in &mut network.hvdc.dc_grids {
                    for converter in &mut grid.converters {
                        if converter.id() != converter_id {
                            continue;
                        }
                        if let Some(lcc) = converter.as_lcc_mut() {
                            lcc.in_service = false;
                        }
                        if let Some(vsc) = converter.as_vsc_mut() {
                            vsc.status = false;
                        }
                        found = true;
                    }
                }
                if !found {
                    return Err(ContingencyModificationError::MissingDcGridConverter {
                        operation: "DcGridConverterTrip",
                        converter_id: converter_id.clone(),
                    });
                }
            }
        }
    }

    Ok(())
}

fn branch_matches(
    br_from: u32,
    br_to: u32,
    br_circuit: &str,
    query_from: u32,
    query_to: u32,
    query_circuit: &str,
) -> bool {
    br_circuit == query_circuit
        && ((br_from == query_from && br_to == query_to)
            || (br_from == query_to && br_to == query_from))
}

/// NERC TPL-001-5.1 contingency category for compliance reporting.
///
/// Used to classify contingencies by their NERC planning category so that
/// the Python TPL checker can route results to the correct P-category handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TplCategory {
    /// Not classified (standard N-1, N-2, or other).
    #[default]
    Unclassified,
    /// P1: Single element outage (line, transformer, generator, shunt, or HVDC pole).
    P1SingleElement,
    /// P2: Single element outage with Remedial Action Scheme (RAS) activation.
    P2SingleWithRAS,
    /// P3: Generator trip (loss of the largest single generating unit).
    P3GeneratorTrip,
    /// P4: Stuck breaker — fault + breaker failure causes loss of entire bus section.
    P4StuckBreaker,
    /// P5: Delayed clearing — extended fault duration due to relay/communication failure.
    P5DelayedClearing,
    /// P6a: Two elements on same tower/structure.
    P6SameTower,
    /// P6b: Two elements in common corridor / right-of-way.
    P6CommonCorridor,
    /// P6c: Two parallel circuits (same from/to bus pair).
    P6ParallelCircuits,
    /// P7: Common-mode outage (two or more elements from a single common cause).
    P7CommonMode,
}

/// A single contingency definition (one or more element outages).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contingency {
    /// Unique identifier (e.g. "branch_42").
    pub id: String,
    /// Human-readable label (e.g. "Line 123->456 (ckt 1)").
    pub label: String,
    /// Branch indices to trip (usually 1 for N-1).
    pub branch_indices: Vec<usize>,
    /// Generator indices to trip (empty for branch-only N-1).
    pub generator_indices: Vec<usize>,
    /// HVDC converter indices to trip (for HVDC contingencies).
    ///
    /// Each index refers to the position in the HVDC link list extracted from
    /// the network (via `hvdc_links_from_network`). Tripping a converter
    /// removes its P/Q injection at both the rectifier and inverter buses.
    #[serde(default)]
    pub hvdc_converter_indices: Vec<usize>,
    /// HVDC cable (DC branch) indices to trip (for HVDC contingencies).
    ///
    /// Tripping a DC cable removes the P injection at both ends of the
    /// corresponding HVDC link, equivalent to setting P_dc = 0 for that link.
    #[serde(default)]
    pub hvdc_cable_indices: Vec<usize>,
    /// Switch mRIDs to toggle for breaker contingencies.
    ///
    /// When non-empty this is a **breaker contingency** that requires topology
    /// re-reduction (via `surge_topology::rebuild_topology`) before power flow.
    /// Each entry is the mRID of a switch to open (for trip contingencies) or
    /// close (for restoration studies).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch_ids: Vec<String>,
    /// NERC TPL-001-5.1 category classification.
    ///
    /// Set by the P4/P5/P6 contingency generators; defaults to `Unclassified`
    /// for standard N-1/N-2 contingencies.
    #[serde(default)]
    pub tpl_category: TplCategory,
    /// Simultaneous network modifications (PSS/E .con SET/CHANGE commands).
    ///
    /// Applied to the per-contingency network clone immediately after element
    /// outages, before the post-contingency power flow is solved.
    /// Empty for standard N-1/N-2 contingencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifications: Vec<ContingencyModification>,
}

impl Default for Contingency {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            branch_indices: vec![],
            generator_indices: vec![],
            hvdc_converter_indices: vec![],
            hvdc_cable_indices: vec![],
            switch_ids: vec![],
            tpl_category: TplCategory::Unclassified,
            modifications: vec![],
        }
    }
}

/// Generate one N-1 contingency per in-service branch.
pub fn generate_n1_branch_contingencies(network: &Network) -> Vec<Contingency> {
    let contingencies: Vec<Contingency> = network
        .branches
        .iter()
        .enumerate()
        .filter(|(_, br)| br.in_service)
        .map(|(i, br)| Contingency {
            id: format!("branch_{i}"),
            label: format!("Line {}->{}(ckt {})", br.from_bus, br.to_bus, br.circuit),
            branch_indices: vec![i],
            tpl_category: TplCategory::P1SingleElement,
            ..Default::default()
        })
        .collect();
    info!(
        buses = network.n_buses(),
        branches = network.n_branches(),
        contingencies = contingencies.len(),
        "generated N-1 branch contingencies"
    );
    contingencies
}

/// Generate one breaker contingency per closed breaker in the [`NodeBreakerTopology`].
///
/// Each contingency opens a single breaker, which may split a bus and change
/// the network topology.  These contingencies require topology re-reduction
/// (via `surge_topology::rebuild_topology`) rather than a simple branch-trip.
pub fn generate_breaker_contingencies(model: &NodeBreakerTopology) -> Vec<Contingency> {
    let contingencies: Vec<Contingency> = model
        .switches
        .iter()
        .filter(|sw| sw.switch_type == SwitchType::Breaker && !sw.open)
        .map(|sw| Contingency {
            id: format!("breaker_{}", sw.id),
            label: format!("Trip breaker {}", sw.name),
            switch_ids: vec![sw.id.clone()],
            ..Default::default()
        })
        .collect();
    info!(
        breakers = contingencies.len(),
        "generated breaker contingencies"
    );
    contingencies
}

// ---------------------------------------------------------------------------
// NERC TPL-001-5.1 Extreme Event Contingency Generators
// ---------------------------------------------------------------------------

/// Generate P4 stuck-breaker contingencies (NERC TPL-001-5.1 Category P4).
///
/// For each in-service branch, simulates a stuck breaker at each endpoint bus.
/// A stuck breaker at bus B means the fault on the initiating element is not
/// cleared locally, so backup protection trips **all** elements connected to
/// that bus section:
/// - All other branches connected to bus B
/// - All generators at bus B
///
/// This produces up to 2 contingencies per branch (one per endpoint).
/// Contingencies are deduplicated by their sorted element sets.
///
/// In a bus-branch model without explicit breaker topology, "bus section"
/// is equivalent to "all elements at the bus number".
pub fn generate_p4_stuck_breaker_contingencies(network: &Network) -> Vec<Contingency> {
    // Build bus → branch adjacency
    let mut bus_to_branches: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        bus_to_branches.entry(br.from_bus).or_default().push(i);
        bus_to_branches.entry(br.to_bus).or_default().push(i);
    }

    // Build bus → generator adjacency
    let mut bus_to_gens: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, g) in network.generators.iter().enumerate() {
        if g.in_service {
            bus_to_gens.entry(g.bus).or_default().push(i);
        }
    }

    // Track unique contingency signatures to avoid duplicates
    let mut seen: std::collections::HashSet<(Vec<usize>, Vec<usize>)> =
        std::collections::HashSet::new();
    let mut contingencies = Vec::new();

    for (i, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }

        // For each endpoint of the faulted branch
        for &bus in &[br.from_bus, br.to_bus] {
            // Collect all branches at this bus (including the initiating one)
            let mut branch_indices: Vec<usize> =
                bus_to_branches.get(&bus).cloned().unwrap_or_default();

            // Ensure the initiating branch is included
            if !branch_indices.contains(&i) {
                branch_indices.push(i);
            }
            branch_indices.sort_unstable();
            branch_indices.dedup();

            // Collect all generators at this bus
            let mut gen_indices: Vec<usize> = bus_to_gens.get(&bus).cloned().unwrap_or_default();
            gen_indices.sort_unstable();

            // Dedup: skip if we've seen this exact combination
            let sig = (branch_indices.clone(), gen_indices.clone());
            if !seen.insert(sig) {
                continue;
            }

            let n_elements = branch_indices.len() + gen_indices.len();
            let branch_labels: Vec<String> = branch_indices
                .iter()
                .map(|&idx| {
                    let b = &network.branches[idx];
                    format!("{}->{}({})", b.from_bus, b.to_bus, b.circuit)
                })
                .collect();

            contingencies.push(Contingency {
                id: format!("p4_br{i}_bus{bus}"),
                label: format!(
                    "P4 stuck breaker bus {bus}: {n_elements} elements [{}]",
                    branch_labels.join(", ")
                ),
                branch_indices,
                generator_indices: gen_indices,
                tpl_category: TplCategory::P4StuckBreaker,
                ..Default::default()
            });
        }
    }

    info!(
        contingencies = contingencies.len(),
        branches = network.n_branches(),
        "generated P4 stuck-breaker contingencies"
    );
    contingencies
}

/// Generate P5 delayed-clearing contingencies (NERC TPL-001-5.1 Category P5).
///
/// Creates one contingency per in-service branch, tagged as
/// [`TplCategory::P5DelayedClearing`]. These contingencies represent
/// single-line faults with protection relay or communication failure,
/// resulting in delayed clearing times (typically 15–30 cycles vs 5 cycles
/// for normal clearing).
///
/// Use with `surge_dyn::p5::run_p5_from_contingencies()` for filtered
/// screening that only simulates delayed clearing on branches stable
/// under normal clearing.
pub fn generate_p5_contingencies(network: &Network) -> Vec<Contingency> {
    let mut contingencies = Vec::new();

    for (i, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        contingencies.push(Contingency {
            id: format!("p5_branch_{i}"),
            label: format!(
                "P5 delayed clearing: {}→{}({})",
                br.from_bus, br.to_bus, br.circuit
            ),
            branch_indices: vec![i],
            tpl_category: TplCategory::P5DelayedClearing,
            ..Default::default()
        });
    }

    info!(
        contingencies = contingencies.len(),
        branches = network.n_branches(),
        "generated P5 delayed-clearing contingencies"
    );
    contingencies
}

/// Generate P6c parallel-circuit contingencies (NERC TPL-001-5.1 Category P6).
///
/// Auto-detects branches sharing the same `(from_bus, to_bus)` bus pair
/// (parallel circuits). For each group of 2+ parallel circuits, generates
/// C(n,2) contingencies tripping each pair simultaneously.
///
/// This is the automatic detection path. For same-tower (P6a) or
/// common-corridor (P6b) pairs that require engineering judgment,
/// use [`generate_p6_user_pairs`] with explicit pair lists.
pub fn generate_p6_parallel_contingencies(network: &Network) -> Vec<Contingency> {
    // Group in-service branches by normalized bus pair
    let mut groups: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, br) in network.branches.iter().enumerate() {
        if !br.in_service {
            continue;
        }
        let key = if br.from_bus <= br.to_bus {
            (br.from_bus, br.to_bus)
        } else {
            (br.to_bus, br.from_bus)
        };
        groups.entry(key).or_default().push(i);
    }

    let mut contingencies = Vec::new();
    for ((bus_lo, bus_hi), indices) in &groups {
        if indices.len() < 2 {
            continue;
        }
        // Generate all C(n,2) pairs
        for (ia, &a) in indices.iter().enumerate() {
            for &b in &indices[ia + 1..] {
                let br_a = &network.branches[a];
                let br_b = &network.branches[b];
                contingencies.push(Contingency {
                    id: format!("p6c_br{a}_{b}"),
                    label: format!(
                        "P6c parallel {bus_lo}->{bus_hi}: ckt {} + ckt {}",
                        br_a.circuit, br_b.circuit
                    ),
                    branch_indices: vec![a, b],
                    tpl_category: TplCategory::P6ParallelCircuits,
                    ..Default::default()
                });
            }
        }
    }

    info!(
        contingencies = contingencies.len(),
        "generated P6c parallel-circuit contingencies"
    );
    contingencies
}

/// Generate P6 contingencies from user-specified branch pairs.
///
/// Used for P6a (same tower) and P6b (common corridor) contingencies
/// that cannot be auto-detected from the network model and require
/// engineering judgment.
///
/// Each entry in `pairs` is `(branch_idx_a, branch_idx_b)`.
/// Invalid indices (out of range or out of service) are silently skipped.
pub fn generate_p6_user_pairs(
    network: &Network,
    pairs: &[(usize, usize)],
    category: TplCategory,
) -> Vec<Contingency> {
    let cat_label = match category {
        TplCategory::P6SameTower => "P6a tower",
        TplCategory::P6CommonCorridor => "P6b corridor",
        _ => "P6 user",
    };
    let n_br = network.branches.len();

    let contingencies: Vec<Contingency> = pairs
        .iter()
        .filter(|&&(a, b)| {
            a < n_br
                && b < n_br
                && a != b
                && network.branches[a].in_service
                && network.branches[b].in_service
        })
        .map(|&(a, b)| {
            let br_a = &network.branches[a];
            let br_b = &network.branches[b];
            Contingency {
                id: format!("p6_{a}_{b}"),
                label: format!(
                    "{cat_label}: {}->{}({}) + {}->{}({})",
                    br_a.from_bus,
                    br_a.to_bus,
                    br_a.circuit,
                    br_b.from_bus,
                    br_b.to_bus,
                    br_b.circuit,
                ),
                branch_indices: vec![a, b],
                tpl_category: category,
                ..Default::default()
            }
        })
        .collect();

    info!(
        contingencies = contingencies.len(),
        pairs_supplied = pairs.len(),
        category = cat_label,
        "generated P6 user-specified contingencies"
    );
    contingencies
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{
        Branch, Bus, BusType, DcBus, DcConverter, DcConverterStation, Generator, HvdcLink,
        LccConverterTerminal, LccHvdcLink, SwitchDevice, SwitchedShunt, VscConverterTerminal,
        VscHvdcLink,
    };

    #[test]
    fn test_contingency_hvdc_fields() {
        let ctg = Contingency {
            id: "hvdc_conv_0".into(),
            label: "Trip HVDC converter 0".into(),
            hvdc_converter_indices: vec![0],
            ..Default::default()
        };
        assert_eq!(ctg.hvdc_converter_indices, vec![0]);
        assert!(ctg.hvdc_cable_indices.is_empty());

        let json = serde_json::to_string(&ctg).unwrap();
        let deser: Contingency = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.hvdc_converter_indices, vec![0]);
        assert!(deser.hvdc_cable_indices.is_empty());

        // Deserialize without HVDC/tpl_category fields (backward compat).
        let json_legacy =
            r#"{"id":"br_0","label":"x","branch_indices":[0],"generator_indices":[]}"#;
        let deser_legacy: Contingency = serde_json::from_str(json_legacy).unwrap();
        assert!(deser_legacy.hvdc_converter_indices.is_empty());
        assert!(deser_legacy.hvdc_cable_indices.is_empty());
        assert_eq!(deser_legacy.tpl_category, TplCategory::Unclassified);
    }

    #[test]
    fn test_contingency_hvdc_cable() {
        let ctg = Contingency {
            id: "hvdc_cable_2".into(),
            label: "Trip HVDC cable 2".into(),
            hvdc_cable_indices: vec![2],
            ..Default::default()
        };
        assert_eq!(ctg.hvdc_cable_indices, vec![2]);
        assert!(ctg.hvdc_converter_indices.is_empty());
    }

    #[test]
    fn test_breaker_contingency_generation() {
        let model = NodeBreakerTopology::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                SwitchDevice {
                    id: "BRK_1".into(),
                    name: "Breaker 1".into(),
                    switch_type: SwitchType::Breaker,
                    cn1_id: "CN_A".into(),
                    cn2_id: "CN_B".into(),
                    open: false,
                    normal_open: false,
                    retained: false,
                    rated_current: None,
                },
                SwitchDevice {
                    id: "BRK_2".into(),
                    name: "Breaker 2".into(),
                    switch_type: SwitchType::Breaker,
                    cn1_id: "CN_C".into(),
                    cn2_id: "CN_D".into(),
                    open: true,
                    normal_open: true,
                    retained: false,
                    rated_current: None,
                },
                SwitchDevice {
                    id: "DIS_1".into(),
                    name: "Disconnector 1".into(),
                    switch_type: SwitchType::Disconnector,
                    cn1_id: "CN_A".into(),
                    cn2_id: "CN_C".into(),
                    open: false,
                    normal_open: false,
                    retained: false,
                    rated_current: None,
                },
            ],
            Vec::new(),
        );

        let ctgs = generate_breaker_contingencies(&model);
        assert_eq!(ctgs.len(), 1, "only 1 closed breaker");
        assert_eq!(ctgs[0].switch_ids, vec!["BRK_1"]);
        assert!(ctgs[0].branch_indices.is_empty());
    }

    #[test]
    fn test_switch_ids_serde_backward_compat() {
        let json = r#"{"id":"br_0","label":"x","branch_indices":[0],"generator_indices":[]}"#;
        let ctg: Contingency = serde_json::from_str(json).unwrap();
        assert!(ctg.switch_ids.is_empty());
        assert_eq!(ctg.tpl_category, TplCategory::Unclassified);

        let ctg2 = Contingency {
            id: "brk_1".into(),
            label: "Trip breaker 1".into(),
            switch_ids: vec!["BRK_1".into()],
            ..Default::default()
        };
        let json2 = serde_json::to_string(&ctg2).unwrap();
        let deser: Contingency = serde_json::from_str(&json2).unwrap();
        assert_eq!(deser.switch_ids, vec!["BRK_1"]);
    }

    #[test]
    fn test_tpl_category_serde_backward_compat() {
        // Old JSON without tpl_category should deserialize with Unclassified.
        let json = r#"{"id":"br_0","label":"x","branch_indices":[0],"generator_indices":[]}"#;
        let ctg: Contingency = serde_json::from_str(json).unwrap();
        assert_eq!(ctg.tpl_category, TplCategory::Unclassified);

        // Round-trip with P4 category.
        let ctg = Contingency {
            id: "p4_br0_bus1".into(),
            label: "P4 stuck breaker".into(),
            branch_indices: vec![0, 1, 2],
            generator_indices: vec![0],
            tpl_category: TplCategory::P4StuckBreaker,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctg).unwrap();
        let deser: Contingency = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.tpl_category, TplCategory::P4StuckBreaker);
        assert_eq!(deser.branch_indices, vec![0, 1, 2]);
        assert_eq!(deser.generator_indices, vec![0]);
    }

    #[test]
    fn test_p4_stuck_breaker_generation() {
        // Build a 3-bus triangle network:
        //   Bus 1 --[br0]--> Bus 2
        //   Bus 2 --[br1]--> Bus 3
        //   Bus 1 --[br2]--> Bus 3
        // Generator at bus 1.
        let mut net = Network::new("p4_test");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 345.0),
            Bus::new(2, BusType::PQ, 345.0),
            Bus::new(3, BusType::PQ, 345.0),
        ];
        net.branches = vec![
            Branch::new_line(1, 2, 0.01, 0.1, 0.0),
            Branch::new_line(2, 3, 0.01, 0.1, 0.0),
            Branch::new_line(1, 3, 0.01, 0.1, 0.0),
        ];
        let mut g = Generator::new(1, 100.0, 0.0);
        g.in_service = true;
        net.generators = vec![g];

        let ctgs = generate_p4_stuck_breaker_contingencies(&net);

        // Bus 1 has br0, br2, gen0 → stuck breaker at bus 1 trips {br0,br2} + {gen0}
        // Bus 2 has br0, br1 → stuck breaker at bus 2 trips {br0,br1}
        // Bus 3 has br1, br2 → stuck breaker at bus 3 trips {br1,br2}
        // For br0 (1→2): bus 1 → {br0,br2,gen0}, bus 2 → {br0,br1}
        // For br1 (2→3): bus 2 → {br0,br1} (already seen), bus 3 → {br1,br2}
        // For br2 (1→3): bus 1 → {br0,br2,gen0} (already seen), bus 3 → {br1,br2} (already seen)
        // Unique: {br0,br2,gen0}, {br0,br1}, {br1,br2} = 3 contingencies

        assert_eq!(
            ctgs.len(),
            3,
            "3-bus triangle should give 3 unique P4 contingencies"
        );

        // All should be P4
        for ctg in &ctgs {
            assert_eq!(ctg.tpl_category, TplCategory::P4StuckBreaker);
            assert!(ctg.branch_indices.len() >= 2, "P4 trips 2+ elements");
        }

        // The bus-1 contingency should include the generator
        let bus1_ctg = ctgs.iter().find(|c| c.id.contains("bus1")).unwrap();
        assert_eq!(bus1_ctg.generator_indices, vec![0]);
    }

    #[test]
    fn test_p6_parallel_detection() {
        // Build a network with parallel circuits between bus 1 and bus 2.
        let mut net = Network::new("p6_test");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 345.0),
            Bus::new(2, BusType::PQ, 345.0),
            Bus::new(3, BusType::PQ, 345.0),
        ];

        let mut br0 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br0.circuit = "1".to_string();
        let mut br1 = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        br1.circuit = "2".to_string();
        let mut br2 = Branch::new_line(2, 3, 0.01, 0.1, 0.0);
        br2.circuit = "1".to_string();
        net.branches = vec![br0, br1, br2];

        let ctgs = generate_p6_parallel_contingencies(&net);

        // Only branches 0 and 1 are parallel (1→2, ckt 1 and ckt 2)
        assert_eq!(ctgs.len(), 1, "one parallel pair between bus 1 and 2");
        assert_eq!(ctgs[0].branch_indices, vec![0, 1]);
        assert_eq!(ctgs[0].tpl_category, TplCategory::P6ParallelCircuits);
    }

    #[test]
    fn test_p6_user_pairs() {
        let mut net = Network::new("p6_user_test");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 345.0),
            Bus::new(2, BusType::PQ, 345.0),
            Bus::new(3, BusType::PQ, 345.0),
        ];
        net.branches = vec![
            Branch::new_line(1, 2, 0.01, 0.1, 0.0),
            Branch::new_line(2, 3, 0.01, 0.1, 0.0),
            Branch::new_line(1, 3, 0.01, 0.1, 0.0),
        ];

        let pairs = vec![(0, 2), (1, 2)];
        let ctgs = generate_p6_user_pairs(&net, &pairs, TplCategory::P6SameTower);
        assert_eq!(ctgs.len(), 2);
        for ctg in &ctgs {
            assert_eq!(ctg.tpl_category, TplCategory::P6SameTower);
            assert_eq!(ctg.branch_indices.len(), 2);
        }

        // Invalid pairs should be silently skipped
        let bad_pairs = vec![(0, 99), (0, 0)]; // out of range, self-pair
        let ctgs = generate_p6_user_pairs(&net, &bad_pairs, TplCategory::P6CommonCorridor);
        assert_eq!(ctgs.len(), 0);
    }

    // -----------------------------------------------------------------------
    // DcLineBlock / VscDcLineBlock / SwitchedShuntRemove
    // -----------------------------------------------------------------------

    fn build_dc_network() -> Network {
        let mut net = Network::new("dc_test");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PQ, 100.0),
        ];
        net.hvdc.links = vec![HvdcLink::Lcc(LccHvdcLink {
            name: "HVDC-1".to_string(),
            mode: LccHvdcControlMode::PowerControl,
            rectifier: LccConverterTerminal {
                bus: 1,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 2,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        })];
        net.hvdc.links.push(HvdcLink::Vsc(VscHvdcLink {
            name: "VSC-1".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            converter1: VscConverterTerminal {
                bus: 1,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                ..VscConverterTerminal::default()
            },
            ..VscHvdcLink::default()
        }));
        net
    }

    fn build_explicit_dc_grid_network() -> Network {
        let mut net = Network::new("explicit_dc_grid");
        net.base_mva = 100.0;
        net.buses = vec![
            Bus::new(1, BusType::Slack, 230.0),
            Bus::new(2, BusType::PQ, 230.0),
        ];

        let grid = net.hvdc.ensure_dc_grid(1, Some("grid".into()));
        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            id: "conv_a".into(),
            dc_bus: 101,
            ac_bus: 1,
            control_type_dc: 2,
            control_type_ac: 1,
            active_power_mw: 0.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 2.0,
            status: true,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 0.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 100.0,
            reactive_power_ac_min_mvar: -100.0,
        }));

        net
    }

    #[test]
    fn dc_line_block_sets_mode_blocked() {
        let mut net = build_dc_network();
        assert_eq!(
            net.hvdc.links[0].as_lcc().unwrap().mode,
            LccHvdcControlMode::PowerControl
        );

        apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::DcLineBlock {
                name: "HVDC-1".into(),
            }],
        )
        .expect("dc line block should succeed");

        assert_eq!(
            net.hvdc.links[0].as_lcc().unwrap().mode,
            LccHvdcControlMode::Blocked,
            "DcLineBlock must set mode to Blocked"
        );
    }

    #[test]
    fn dc_line_block_unknown_name_errors() {
        let mut net = build_dc_network();
        let err = apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::DcLineBlock {
                name: "NONEXISTENT".into(),
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContingencyModificationError::MissingHvdcLink { .. }
        ));
    }

    #[test]
    fn branch_close_rejects_missing_branch() {
        let mut net = build_dc_network();
        let err = apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::BranchClose {
                from_bus: 9,
                to_bus: 10,
                circuit: "1".into(),
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContingencyModificationError::MissingBranch {
                operation: "BranchClose",
                from_bus: 9,
                to_bus: 10,
                circuit,
            } if circuit == "1"
        ));
    }

    #[test]
    fn vsc_dc_line_block_sets_mode_blocked() {
        let mut net = build_dc_network();
        assert_eq!(
            net.hvdc.links[1].as_vsc().unwrap().mode,
            VscHvdcControlMode::PowerControl
        );

        apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::VscDcLineBlock {
                name: "VSC-1".into(),
            }],
        )
        .expect("vsc dc line block should succeed");

        assert_eq!(
            net.hvdc.links[1].as_vsc().unwrap().mode,
            VscHvdcControlMode::Blocked,
            "VscDcLineBlock must set mode to Blocked"
        );
    }

    #[test]
    fn dc_grid_converter_trip_sets_converter_out_of_service() {
        let mut net = build_explicit_dc_grid_network();
        assert!(net.hvdc.dc_grids[0].converters[0].is_in_service());

        apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::DcGridConverterTrip {
                converter_id: "conv_a".into(),
            }],
        )
        .expect("dc-grid converter trip should succeed");

        assert!(
            !net.hvdc.dc_grids[0].converters[0].is_in_service(),
            "DcGridConverterTrip must disable the canonical converter"
        );
    }

    #[test]
    fn dc_line_block_serde_roundtrip() {
        let m = ContingencyModification::DcLineBlock {
            name: "HVDC-TEST".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            json.contains(r#""type":"DcLineBlock""#),
            "serde must produce tagged JSON"
        );
        let back: ContingencyModification = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ContingencyModification::DcLineBlock { name } if name == "HVDC-TEST")
        );
    }

    #[test]
    fn vsc_dc_line_block_serde_roundtrip() {
        let m = ContingencyModification::VscDcLineBlock {
            name: "VSC-TEST".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""type":"VscDcLineBlock""#));
        let back: ContingencyModification = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ContingencyModification::VscDcLineBlock { name } if name == "VSC-TEST")
        );
    }

    #[test]
    fn switched_shunt_remove_serde_roundtrip() {
        let m = ContingencyModification::SwitchedShuntRemove { bus: 42 };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""type":"SwitchedShuntRemove""#));
        let back: ContingencyModification = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ContingencyModification::SwitchedShuntRemove { bus: 42 }
        ));
    }

    /// SwitchedShuntRemove must zero out the discrete SwitchedShunt at the target
    /// bus without touching bus.shunt_susceptance_mvar (the shunt's BINIT was never baked in).
    #[test]
    fn switched_shunt_remove_uses_switched_shunts_field() {
        let mut net = Network::new("test");
        net.base_mva = 100.0;
        net.buses = vec![Bus::new(5, BusType::Slack, 100.0)];
        // Controlled shunt at bus 5 — contribution NOT in bus.shunt_susceptance_mvar.
        net.buses[0].shunt_susceptance_mvar = 0.0; // explicitly zero
        net.controls.switched_shunts = vec![SwitchedShunt {
            id: "ssh_5".into(),
            bus: 5,
            bus_regulated: 5,
            b_step: 0.5, // 50 Mvar / 100 MVA
            n_steps_cap: 4,
            n_steps_react: 0,
            v_target: 1.0,
            v_band: 0.1,
            n_active_steps: 3,
        }];

        let mods = vec![ContingencyModification::SwitchedShuntRemove { bus: 5 }];
        apply_contingency_modifications(&mut net, &mods)
            .expect("switched shunt removal should succeed");

        // Steps zeroed out — shunt is disabled.
        assert_eq!(net.controls.switched_shunts[0].n_steps_cap, 0);
        assert_eq!(net.controls.switched_shunts[0].n_steps_react, 0);
        assert_eq!(net.controls.switched_shunts[0].n_active_steps, 0);

        // bus.shunt_susceptance_mvar must not have changed (controlled shunt was never baked in).
        assert!(net.buses[0].shunt_susceptance_mvar.abs() < 1e-9);
    }

    #[test]
    fn shunt_adjust_scales_by_base_mva() {
        let mut net = Network::new("test");
        net.base_mva = 50.0;
        net.buses = vec![Bus::new(5, BusType::Slack, 100.0)];
        net.buses[0].shunt_susceptance_mvar = 12.0;

        apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::ShuntAdjust {
                bus: 5,
                delta_b_pu: 0.5,
            }],
        )
        .expect("shunt adjust should succeed");

        assert!((net.buses[0].shunt_susceptance_mvar - 37.0).abs() < 1e-9);
    }

    #[test]
    fn branch_modifications_are_direction_insensitive() {
        let mut net = Network::new("test");
        net.buses = vec![
            Bus::new(1, BusType::Slack, 100.0),
            Bus::new(2, BusType::PQ, 100.0),
        ];
        net.branches = vec![crate::network::Branch::new_line(1, 2, 0.01, 0.1, 0.0)];
        net.branches[0].in_service = false;
        net.branches[0].tap = 1.02;

        apply_contingency_modifications(
            &mut net,
            &[
                ContingencyModification::BranchClose {
                    from_bus: 2,
                    to_bus: 1,
                    circuit: "1".to_string(),
                },
                ContingencyModification::BranchTap {
                    from_bus: 2,
                    to_bus: 1,
                    circuit: "1".to_string(),
                    tap: 1.08,
                },
            ],
        )
        .expect("branch modifications should succeed");

        assert!(net.branches[0].in_service);
        assert!((net.branches[0].tap - 1.08).abs() < 1e-12);
    }

    #[test]
    fn load_set_rejects_missing_bus() {
        let mut net = build_dc_network();
        let err = apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::LoadSet {
                bus: 99,
                p_mw: 10.0,
                q_mvar: 2.0,
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContingencyModificationError::MissingBus {
                operation: "LoadSet",
                bus: 99
            }
        ));
    }

    #[test]
    fn load_adjust_rejects_missing_bus() {
        let mut net = build_dc_network();
        let err = apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::LoadAdjust {
                bus: 99,
                delta_p_mw: 1.0,
                delta_q_mvar: 0.5,
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContingencyModificationError::MissingBus {
                operation: "LoadAdjust",
                bus: 99
            }
        ));
    }

    #[test]
    fn gen_output_set_rejects_missing_generator() {
        let mut net = build_dc_network();
        let err = apply_contingency_modifications(
            &mut net,
            &[ContingencyModification::GenOutputSet {
                bus: 1,
                machine_id: "9".into(),
                p_mw: 42.0,
            }],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ContingencyModificationError::MissingGenerator {
                operation: "GenOutputSet",
                bus: 1,
                machine_id,
            } if machine_id == "9"
        ));
    }
}
