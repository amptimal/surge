// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Network case diff utility.
//!
//! Compares two [`Network`] objects and produces a structured diff showing
//! buses, branches, and generators that were added, removed, or modified.
//! Useful for verifying parser round-trips and inspecting contingency effects.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::network::{Branch, Generator, Load, Network};
/// Kind of change detected for an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffKind {
    Added,
    Removed,
    Modified,
}

/// Diff entry for a bus. Modified fields are `Some` with `(old, new)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusDiff {
    pub bus_number: u32,
    pub kind: DiffKind,
    pub bus_type: Option<(String, String)>,
    pub voltage_magnitude_pu: Option<(f64, f64)>,
    pub voltage_angle_rad: Option<(f64, f64)>,
    pub base_kv: Option<(f64, f64)>,
}

/// Diff entry for a branch. Modified fields are `Some` with `(old, new)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchDiff {
    pub from_bus: u32,
    pub to_bus: u32,
    pub circuit: String,
    pub kind: DiffKind,
    pub r: Option<(f64, f64)>,
    pub x: Option<(f64, f64)>,
    pub b: Option<(f64, f64)>,
    pub rating_a_mva: Option<(f64, f64)>,
    pub tap: Option<(f64, f64)>,
    pub phase_shift_rad: Option<(f64, f64)>,
    pub in_service: Option<(bool, bool)>,
}

/// Diff entry for a generator. Modified fields are `Some` with `(old, new)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenDiff {
    pub bus: u32,
    pub id: String,
    pub kind: DiffKind,
    pub p: Option<(f64, f64)>,
    pub q: Option<(f64, f64)>,
    pub pmin: Option<(f64, f64)>,
    pub pmax: Option<(f64, f64)>,
    pub qmin: Option<(f64, f64)>,
    pub qmax: Option<(f64, f64)>,
    pub in_service: Option<(bool, bool)>,
    /// Cost change shown as debug strings when the cost curves differ.
    pub cost: Option<(String, String)>,
}

/// Diff entry for a load. Modified fields are `Some` with `(old, new)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadDiff {
    pub bus: u32,
    pub id: String,
    pub kind: DiffKind,
    pub active_power_demand_mw: Option<(f64, f64)>,
    pub reactive_power_demand_mvar: Option<(f64, f64)>,
    pub in_service: Option<(bool, bool)>,
}

/// Structured diff between two networks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseDiff {
    pub bus_diffs: Vec<BusDiff>,
    pub branch_diffs: Vec<BranchDiff>,
    pub gen_diffs: Vec<GenDiff>,
    pub load_diffs: Vec<LoadDiff>,
    pub summary: String,
}

/// Compare two `f64` values; return `Some((a, b))` if they differ.
#[inline]
fn diff_f64(a: f64, b: f64) -> Option<(f64, f64)> {
    if (a - b).abs() > f64::EPSILON {
        Some((a, b))
    } else {
        None
    }
}

/// Produce a structured diff between two [`Network`] objects.
///
/// Buses are matched by `bus_number`, branches by `(from_bus, to_bus, circuit)`,
/// and generators by canonical generator ID, synthesizing missing IDs
/// deterministically from bus order when needed.
pub fn diff_networks(a: &Network, b: &Network) -> CaseDiff {
    let bus_diffs = diff_buses(a, b);
    let branch_diffs = diff_branches(a, b);
    let gen_diffs = diff_gens(a, b);
    let load_diffs = diff_loads(a, b);

    let mut parts = Vec::new();

    for (name, total, added, removed, modified) in [
        (
            "bus",
            bus_diffs.len(),
            bus_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Added)
                .count(),
            bus_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Removed)
                .count(),
            bus_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Modified)
                .count(),
        ),
        (
            "branch",
            branch_diffs.len(),
            branch_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Added)
                .count(),
            branch_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Removed)
                .count(),
            branch_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Modified)
                .count(),
        ),
        (
            "generator",
            gen_diffs.len(),
            gen_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Added)
                .count(),
            gen_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Removed)
                .count(),
            gen_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Modified)
                .count(),
        ),
        (
            "load",
            load_diffs.len(),
            load_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Added)
                .count(),
            load_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Removed)
                .count(),
            load_diffs
                .iter()
                .filter(|d| d.kind == DiffKind::Modified)
                .count(),
        ),
    ] {
        if total > 0 {
            let mut sub = Vec::new();
            if added > 0 {
                sub.push(format!("{added} added"));
            }
            if removed > 0 {
                sub.push(format!("{removed} removed"));
            }
            if modified > 0 {
                sub.push(format!("{modified} modified"));
            }
            let plural = if total == 1 {
                ""
            } else if name == "branch" {
                "es"
            } else {
                "s"
            };
            parts.push(format!("{total} {name}{plural} ({})", sub.join(", ")));
        }
    }

    let summary = if parts.is_empty() {
        "no differences".to_string()
    } else {
        parts.join("; ")
    };

    CaseDiff {
        bus_diffs,
        branch_diffs,
        gen_diffs,
        load_diffs,
        summary,
    }
}

fn diff_buses(a: &Network, b: &Network) -> Vec<BusDiff> {
    let a_map: HashMap<u32, _> = a.buses.iter().map(|bus| (bus.number, bus)).collect();
    let b_map: HashMap<u32, _> = b.buses.iter().map(|bus| (bus.number, bus)).collect();
    let mut diffs = Vec::new();

    for (&num, ba) in &a_map {
        if let Some(bb) = b_map.get(&num) {
            let bus_type = if ba.bus_type != bb.bus_type {
                Some((format!("{:?}", ba.bus_type), format!("{:?}", bb.bus_type)))
            } else {
                None
            };
            let vm = diff_f64(ba.voltage_magnitude_pu, bb.voltage_magnitude_pu);
            let va = diff_f64(ba.voltage_angle_rad, bb.voltage_angle_rad);
            let base_kv = diff_f64(ba.base_kv, bb.base_kv);
            if bus_type.is_some() || vm.is_some() || va.is_some() || base_kv.is_some() {
                diffs.push(BusDiff {
                    bus_number: num,
                    kind: DiffKind::Modified,
                    bus_type,
                    voltage_magnitude_pu: vm,
                    voltage_angle_rad: va,
                    base_kv,
                });
            }
        } else {
            diffs.push(BusDiff {
                bus_number: num,
                kind: DiffKind::Removed,
                bus_type: None,
                voltage_magnitude_pu: None,
                voltage_angle_rad: None,
                base_kv: None,
            });
        }
    }
    for &num in b_map.keys() {
        if !a_map.contains_key(&num) {
            diffs.push(BusDiff {
                bus_number: num,
                kind: DiffKind::Added,
                bus_type: None,
                voltage_magnitude_pu: None,
                voltage_angle_rad: None,
                base_kv: None,
            });
        }
    }
    diffs.sort_by_key(|d| d.bus_number);
    diffs
}

fn diff_branches(a: &Network, b: &Network) -> Vec<BranchDiff> {
    type Key = (u32, u32, String);
    let key_of = |br: &Branch| -> Key { (br.from_bus, br.to_bus, br.circuit.clone()) };

    let a_map: HashMap<Key, _> = a.branches.iter().map(|br| (key_of(br), br)).collect();
    let b_map: HashMap<Key, _> = b.branches.iter().map(|br| (key_of(br), br)).collect();
    let mut diffs = Vec::new();

    for (key, ba) in &a_map {
        if let Some(bb) = b_map.get(key) {
            let r = diff_f64(ba.r, bb.r);
            let x = diff_f64(ba.x, bb.x);
            let b = diff_f64(ba.b, bb.b);
            let rating_a_mva = diff_f64(ba.rating_a_mva, bb.rating_a_mva);
            let tap = diff_f64(ba.tap, bb.tap);
            let phase_shift_rad = diff_f64(ba.phase_shift_rad, bb.phase_shift_rad);
            let in_service = if ba.in_service != bb.in_service {
                Some((ba.in_service, bb.in_service))
            } else {
                None
            };
            if r.is_some()
                || x.is_some()
                || b.is_some()
                || rating_a_mva.is_some()
                || tap.is_some()
                || phase_shift_rad.is_some()
                || in_service.is_some()
            {
                diffs.push(BranchDiff {
                    from_bus: key.0,
                    to_bus: key.1,
                    circuit: key.2.clone(),
                    kind: DiffKind::Modified,
                    r,
                    x,
                    b,
                    rating_a_mva,
                    tap,
                    phase_shift_rad,
                    in_service,
                });
            }
        } else {
            diffs.push(BranchDiff {
                from_bus: key.0,
                to_bus: key.1,
                circuit: key.2.clone(),
                kind: DiffKind::Removed,
                r: None,
                x: None,
                b: None,
                rating_a_mva: None,
                tap: None,
                phase_shift_rad: None,
                in_service: None,
            });
        }
    }
    for key in b_map.keys() {
        if !a_map.contains_key(key) {
            diffs.push(BranchDiff {
                from_bus: key.0,
                to_bus: key.1,
                circuit: key.2.clone(),
                kind: DiffKind::Added,
                r: None,
                x: None,
                b: None,
                rating_a_mva: None,
                tap: None,
                phase_shift_rad: None,
                in_service: None,
            });
        }
    }
    diffs.sort_by_key(|d| (d.from_bus, d.to_bus, d.circuit.clone()));
    diffs
}

fn diff_gens(a: &Network, b: &Network) -> Vec<GenDiff> {
    let a_map = canonical_generator_map(&a.generators);
    let b_map = canonical_generator_map(&b.generators);
    let mut diffs = Vec::new();

    for (key, ga) in &a_map {
        if let Some(gb) = b_map.get(key) {
            let p = diff_f64(ga.p, gb.p);
            let q = diff_f64(ga.q, gb.q);
            let pmin = diff_f64(ga.pmin, gb.pmin);
            let pmax = diff_f64(ga.pmax, gb.pmax);
            let qmin = diff_f64(ga.qmin, gb.qmin);
            let qmax = diff_f64(ga.qmax, gb.qmax);
            let in_service = if ga.in_service != gb.in_service {
                Some((ga.in_service, gb.in_service))
            } else {
                None
            };
            let cost = {
                let ca = format!("{:?}", ga.cost);
                let cb = format!("{:?}", gb.cost);
                if ca != cb { Some((ca, cb)) } else { None }
            };
            if p.is_some()
                || q.is_some()
                || pmin.is_some()
                || pmax.is_some()
                || qmin.is_some()
                || qmax.is_some()
                || in_service.is_some()
                || cost.is_some()
            {
                diffs.push(GenDiff {
                    bus: ga.bus,
                    id: key.clone(),
                    kind: DiffKind::Modified,
                    p,
                    q,
                    pmin,
                    pmax,
                    qmin,
                    qmax,
                    in_service,
                    cost,
                });
            }
        } else {
            diffs.push(GenDiff {
                bus: ga.bus,
                id: key.clone(),
                kind: DiffKind::Removed,
                p: None,
                q: None,
                pmin: None,
                pmax: None,
                qmin: None,
                qmax: None,
                in_service: None,
                cost: None,
            });
        }
    }
    for key in b_map.keys() {
        if !a_map.contains_key(key) {
            let gb = &b_map[key];
            diffs.push(GenDiff {
                bus: gb.bus,
                id: key.clone(),
                kind: DiffKind::Added,
                p: None,
                q: None,
                pmin: None,
                pmax: None,
                qmin: None,
                qmax: None,
                in_service: None,
                cost: None,
            });
        }
    }
    diffs.sort_by_key(|d| (d.bus, d.id.clone()));
    diffs
}

fn canonical_generator_map(generators: &[Generator]) -> HashMap<String, &Generator> {
    let mut used_ids = HashSet::new();
    for generator in generators {
        let trimmed = generator.id.trim();
        if !trimmed.is_empty() {
            used_ids.insert(trimmed.to_string());
        }
    }

    let mut ordinal_by_bus: HashMap<u32, usize> = HashMap::new();
    let mut map = HashMap::new();

    for generator in generators {
        let trimmed = generator.id.trim();
        let key = if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            let ordinal = ordinal_by_bus.entry(generator.bus).or_insert(0);
            *ordinal += 1;
            let base = format!("gen_{}_{}", generator.bus, *ordinal);
            let mut candidate = base.clone();
            let mut collision = 2usize;
            while used_ids.contains(&candidate) {
                candidate = format!("{base}_{collision}");
                collision += 1;
            }
            used_ids.insert(candidate.clone());
            candidate
        };
        map.entry(key).or_insert(generator);
    }

    map
}

fn diff_loads(a: &Network, b: &Network) -> Vec<LoadDiff> {
    type Key = (u32, String);
    let key_of = |l: &Load| -> Key { (l.bus, l.id.clone()) };

    let a_map: HashMap<Key, _> = a.loads.iter().map(|l| (key_of(l), l)).collect();
    let b_map: HashMap<Key, _> = b.loads.iter().map(|l| (key_of(l), l)).collect();
    let mut diffs = Vec::new();

    for (key, la) in &a_map {
        if let Some(lb) = b_map.get(key) {
            let p = diff_f64(la.active_power_demand_mw, lb.active_power_demand_mw);
            let q = diff_f64(la.reactive_power_demand_mvar, lb.reactive_power_demand_mvar);
            let in_service = if la.in_service != lb.in_service {
                Some((la.in_service, lb.in_service))
            } else {
                None
            };
            if p.is_some() || q.is_some() || in_service.is_some() {
                diffs.push(LoadDiff {
                    bus: key.0,
                    id: key.1.clone(),
                    kind: DiffKind::Modified,
                    active_power_demand_mw: p,
                    reactive_power_demand_mvar: q,
                    in_service,
                });
            }
        } else {
            diffs.push(LoadDiff {
                bus: key.0,
                id: key.1.clone(),
                kind: DiffKind::Removed,
                active_power_demand_mw: None,
                reactive_power_demand_mvar: None,
                in_service: None,
            });
        }
    }
    for key in b_map.keys() {
        if !a_map.contains_key(key) {
            diffs.push(LoadDiff {
                bus: key.0,
                id: key.1.clone(),
                kind: DiffKind::Added,
                active_power_demand_mw: None,
                reactive_power_demand_mvar: None,
                in_service: None,
            });
        }
    }
    diffs.sort_by_key(|d| (d.bus, d.id.clone()));
    diffs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 2-bus, 1-branch, 1-gen network via JSON round-trip.
    /// This avoids listing every struct field explicitly.
    fn minimal_network() -> Network {
        let json = r#"{
            "name":"test","base_mva":100.0,"freq_hz":60.0,
            "buses":[
                {"number":1,"name":"Bus1","bus_type":"Slack",
                 "shunt_conductance_mw":0.0,"shunt_susceptance_mvar":0.0,"area":1,"voltage_magnitude_pu":1.0,"voltage_angle_rad":0.0,"base_kv":230.0,
                 "zone":1,"voltage_max_pu":1.1,"voltage_min_pu":0.9,"island_id":0},
                {"number":2,"name":"Bus2","bus_type":"PQ",
                 "shunt_conductance_mw":0.0,"shunt_susceptance_mvar":0.0,"area":1,"voltage_magnitude_pu":1.0,"voltage_angle_rad":0.0,"base_kv":230.0,
                 "zone":1,"voltage_max_pu":1.1,"voltage_min_pu":0.9,"island_id":0}
            ],
            "branches":[
                {"from_bus":1,"to_bus":2,"circuit":"1","r":0.01,"x":0.1,"b":0.02,
                 "rating_a_mva":100.0,"rating_b_mva":100.0,"rating_c_mva":100.0,"tap":1.0,"phase_shift_rad":0.0,
                 "in_service":true}
            ],
            "loads":[
                {"bus":2,"id":"load_2_1","active_power_demand_mw":50.0,"reactive_power_demand_mvar":20.0,"in_service":true}
            ],
            "generators":[
                {"bus":1,"machine_id":"1","pg":100.0,"qg":0.0,"qmax":100.0,
                 "qmin":-100.0,"voltage_setpoint_pu":1.0,"machine_base_mva":100.0,"pmax":200.0,"pmin":0.0,
                 "in_service":true}
            ]
        }"#;
        serde_json::from_str(json).expect("test network JSON must parse")
    }

    #[test]
    fn test_identical_networks() {
        let net = minimal_network();
        let diff = diff_networks(&net, &net);
        assert!(diff.bus_diffs.is_empty());
        assert!(diff.branch_diffs.is_empty());
        assert!(diff.gen_diffs.is_empty());
        assert_eq!(diff.summary, "no differences");
    }

    #[test]
    fn test_modified_bus_type() {
        let a = minimal_network();
        let mut b = a.clone();
        b.buses[1].bus_type = crate::network::BusType::PV;
        let diff = diff_networks(&a, &b);
        assert_eq!(diff.bus_diffs.len(), 1);
        assert_eq!(diff.bus_diffs[0].kind, DiffKind::Modified);
        assert!(diff.bus_diffs[0].bus_type.is_some());
    }

    #[test]
    fn test_branch_removed() {
        let a = minimal_network();
        let mut b = a.clone();
        b.branches.clear();
        let diff = diff_networks(&a, &b);
        assert_eq!(diff.branch_diffs.len(), 1);
        assert_eq!(diff.branch_diffs[0].kind, DiffKind::Removed);
        assert!(diff.summary.contains("removed"));
    }

    #[test]
    fn test_generator_added() {
        let a = minimal_network();
        let mut b = a.clone();
        let mut g2 = b.generators[0].clone();
        g2.bus = 2;
        g2.machine_id = Some("1".into());
        b.generators.push(g2);
        let diff = diff_networks(&a, &b);
        assert_eq!(diff.gen_diffs.len(), 1);
        assert_eq!(diff.gen_diffs[0].kind, DiffKind::Added);
        assert_eq!(diff.gen_diffs[0].bus, 2);
    }

    #[test]
    fn test_generator_diff_with_missing_ids_keeps_distinct_entries() {
        let mut a = minimal_network();
        let mut b = minimal_network();

        let g2 = Generator::new(1, 20.0, 1.0);
        a.generators.push(g2.clone());
        b.generators.push(g2);
        b.generators[1].p = 25.0;

        let diff = diff_networks(&a, &b);
        assert_eq!(diff.gen_diffs.len(), 1);
        assert_eq!(diff.gen_diffs[0].kind, DiffKind::Modified);
        assert_eq!(diff.gen_diffs[0].bus, 1);
        assert!(diff.gen_diffs[0].id.starts_with("gen_1_2"));
    }
}
