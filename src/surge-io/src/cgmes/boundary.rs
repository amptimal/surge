// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! ENTSO-E Boundary (EQBD/BD) profile parser.
//!
//! Extracts `BoundaryPoint`, `ModelAuthoritySet`, `EquivalentNetwork`,
//! `EquivalentBranch`, and `EquivalentShunt` from the CGMES object store
//! and populates `Network.cim.boundary_data`.

use surge_network::Network;
use surge_network::network::boundary::{
    BoundaryData, BoundaryPoint, EquivalentBranchData, EquivalentNetworkData, EquivalentShuntData,
    ModelAuthoritySet,
};

use super::indices::CgmesIndices;
use super::types::ObjMap;

/// Build boundary data from CGMES objects and attach it to the network.
pub(crate) fn build_boundary_data(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    let mut data = BoundaryData::default();

    // --- BoundaryPoint ---
    for (id, obj) in objects.iter().filter(|(_, o)| o.class == "BoundaryPoint") {
        let cn_mrid = obj.get_ref("ConnectivityNode").map(|s| s.to_string());

        // Resolve CN → TN → bus via the topology reduction
        let bus = cn_mrid.as_deref().and_then(|cn_id| {
            // Try direct CN→TN lookup: in CGMES, ConnectivityNode.TopologicalNode
            let tn = objects
                .get(cn_id)
                .and_then(|cn_obj| cn_obj.get_ref("TopologicalNode"))
                .or_else(|| {
                    // Reverse lookup: find a TopologicalNode that references this CN
                    // via TopologicalNode.ConnectivityNodes or
                    // ConnectivityNode.TopologicalNode
                    idx.tn_ids.iter().find_map(|tn_id| {
                        let tn_obj = objects.get(tn_id.as_str())?;
                        // Check if this TN references our CN
                        if tn_obj.get_ref("ConnectivityNodes") == Some(cn_id)
                            || tn_obj.get_ref("ConnectivityNodeContainer") == Some(cn_id)
                        {
                            Some(tn_id.as_str())
                        } else {
                            None
                        }
                    })
                });
            tn.and_then(|tn_id| idx.tn_bus(tn_id))
        });

        let parse_bool = |key: &str| -> bool {
            obj.get_text(key)
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false)
        };

        data.boundary_points.push(BoundaryPoint {
            mrid: id.clone(),
            connectivity_node_mrid: cn_mrid,
            from_end_iso_code: obj.get_text("fromEndIsoCode").map(|s| s.to_string()),
            to_end_iso_code: obj.get_text("toEndIsoCode").map(|s| s.to_string()),
            from_end_name: obj.get_text("fromEndName").map(|s| s.to_string()),
            to_end_name: obj.get_text("toEndName").map(|s| s.to_string()),
            from_end_name_tso: obj.get_text("fromEndNameTso").map(|s| s.to_string()),
            to_end_name_tso: obj.get_text("toEndNameTso").map(|s| s.to_string()),
            is_direct_current: parse_bool("isDirectCurrent"),
            is_excluded_from_area_interchange: parse_bool("isExcludedFromAreaInterchange"),
            bus,
        });
    }

    // --- ModelAuthoritySet ---
    // First pass: collect all MAS objects
    let mas_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "ModelAuthoritySet")
        .map(|(id, _)| id.clone())
        .collect();

    for mas_id in &mas_ids {
        let obj = &objects[mas_id];

        // Reverse-lookup: find all equipment that references this MAS
        let members: Vec<String> = objects
            .iter()
            .filter(|(_, o)| {
                o.get_ref("ModelAuthoritySet")
                    .map(|r| r == mas_id)
                    .unwrap_or(false)
            })
            .map(|(eq_id, _)| eq_id.clone())
            .collect();

        data.model_authority_sets.push(ModelAuthoritySet {
            mrid: mas_id.clone(),
            name: obj.get_text("name").unwrap_or("unknown").to_string(),
            description: obj.get_text("description").map(|s| s.to_string()),
            members,
        });
    }

    // --- EquivalentNetwork ---
    for (id, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentNetwork")
    {
        data.equivalent_networks.push(EquivalentNetworkData {
            mrid: id.clone(),
            name: obj.get_text("name").unwrap_or("unknown").to_string(),
            description: obj.get_text("description").map(|s| s.to_string()),
            region_mrid: obj.get_ref("Region").map(|s| s.to_string()),
        });
    }

    // --- EquivalentBranch ---
    for (id, obj) in objects
        .iter()
        .filter(|(_, o)| o.class == "EquivalentBranch")
    {
        // Resolve terminals → buses
        let terminals = idx.terminals(id);
        let (from_bus, to_bus) = match terminals.len() {
            0 => (None, None),
            1 => {
                let b = idx
                    .terminal_tn(objects, &terminals[0])
                    .and_then(|tn| idx.tn_bus(tn));
                (b, None)
            }
            _ => {
                let b1 = idx
                    .terminal_tn(objects, &terminals[0])
                    .and_then(|tn| idx.tn_bus(tn));
                let b2 = idx
                    .terminal_tn(objects, &terminals[1])
                    .and_then(|tn| idx.tn_bus(tn));
                (b1, b2)
            }
        };

        data.equivalent_branches.push(EquivalentBranchData {
            mrid: id.clone(),
            network_mrid: obj.get_ref("EquivalentNetwork").map(|s| s.to_string()),
            r_ohm: obj.parse_f64("r").unwrap_or(0.0),
            x_ohm: obj.parse_f64("x").unwrap_or(0.0),
            r0_ohm: obj.parse_f64("r0"),
            x0_ohm: obj.parse_f64("x0"),
            r2_ohm: obj
                .parse_f64("r21")
                .or_else(|| obj.parse_f64("negativeR21")),
            x2_ohm: obj
                .parse_f64("x21")
                .or_else(|| obj.parse_f64("negativeX21")),
            from_bus,
            to_bus,
        });
    }

    // --- EquivalentShunt ---
    for (id, obj) in objects.iter().filter(|(_, o)| o.class == "EquivalentShunt") {
        let terminals = idx.terminals(id);
        let bus = terminals
            .first()
            .and_then(|tid| idx.terminal_tn(objects, tid))
            .and_then(|tn| idx.tn_bus(tn));

        data.equivalent_shunts.push(EquivalentShuntData {
            mrid: id.clone(),
            network_mrid: obj.get_ref("EquivalentNetwork").map(|s| s.to_string()),
            g_s: obj.parse_f64("g").unwrap_or(0.0),
            b_s: obj.parse_f64("b").unwrap_or(0.0),
            bus,
        });
    }

    if !data.is_empty() {
        tracing::info!(
            boundary_points = data.boundary_points.len(),
            model_authority_sets = data.model_authority_sets.len(),
            equivalent_networks = data.equivalent_networks.len(),
            equivalent_branches = data.equivalent_branches.len(),
            equivalent_shunts = data.equivalent_shunts.len(),
            "CGMES boundary data parsed"
        );
        network.cim.boundary_data = data;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgmes::types::{CimObj, CimVal};
    use std::collections::HashMap;

    fn make_obj(class: &str, attrs: &[(&str, CimVal)]) -> CimObj {
        let mut obj = CimObj::new(class);
        for (k, v) in attrs {
            obj.attrs.insert(k.to_string(), v.clone());
        }
        obj
    }

    fn text(s: &str) -> CimVal {
        CimVal::Text(s.to_string())
    }

    fn refv(s: &str) -> CimVal {
        CimVal::Ref(s.to_string())
    }

    #[test]
    fn test_boundary_point_parsing() {
        let mut objects: ObjMap = HashMap::new();
        objects.insert(
            "bp1".to_string(),
            make_obj(
                "BoundaryPoint",
                &[
                    ("fromEndIsoCode", text("DE")),
                    ("toEndIsoCode", text("FR")),
                    ("fromEndName", text("TenneT")),
                    ("toEndName", text("RTE")),
                    ("isDirectCurrent", text("false")),
                    ("isExcludedFromAreaInterchange", text("true")),
                    ("ConnectivityNode", refv("cn1")),
                ],
            ),
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.boundary_data.boundary_points.len(), 1);
        let bp = &network.cim.boundary_data.boundary_points[0];
        assert_eq!(bp.mrid, "bp1");
        assert_eq!(bp.from_end_iso_code.as_deref(), Some("DE"));
        assert_eq!(bp.to_end_iso_code.as_deref(), Some("FR"));
        assert_eq!(bp.from_end_name.as_deref(), Some("TenneT"));
        assert_eq!(bp.to_end_name.as_deref(), Some("RTE"));
        assert!(!bp.is_direct_current);
        assert!(bp.is_excluded_from_area_interchange);
        assert_eq!(bp.connectivity_node_mrid.as_deref(), Some("cn1"));
        // bus is None since no topology reduction exists
        assert!(bp.bus.is_none());
    }

    #[test]
    fn test_model_authority_set_with_members() {
        let mut objects: ObjMap = HashMap::new();
        objects.insert(
            "mas1".to_string(),
            make_obj(
                "ModelAuthoritySet",
                &[
                    ("name", text("TenneT_TSO")),
                    ("description", text("TenneT TSO BV")),
                ],
            ),
        );
        // Equipment that references MAS
        objects.insert(
            "gen1".to_string(),
            make_obj("SynchronousMachine", &[("ModelAuthoritySet", refv("mas1"))]),
        );
        objects.insert(
            "line1".to_string(),
            make_obj("ACLineSegment", &[("ModelAuthoritySet", refv("mas1"))]),
        );
        // Equipment referencing a different MAS — should not appear
        objects.insert(
            "gen2".to_string(),
            make_obj(
                "SynchronousMachine",
                &[("ModelAuthoritySet", refv("mas_other"))],
            ),
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.boundary_data.model_authority_sets.len(), 1);
        let mas = &network.cim.boundary_data.model_authority_sets[0];
        assert_eq!(mas.name, "TenneT_TSO");
        assert_eq!(mas.description.as_deref(), Some("TenneT TSO BV"));
        assert_eq!(mas.members.len(), 2);
        assert!(mas.members.contains(&"gen1".to_string()));
        assert!(mas.members.contains(&"line1".to_string()));
    }

    #[test]
    fn test_equivalent_network_parsing() {
        let mut objects: ObjMap = HashMap::new();
        objects.insert(
            "eqnet1".to_string(),
            make_obj(
                "EquivalentNetwork",
                &[
                    ("name", text("External_FR")),
                    ("description", text("French external equivalent")),
                    ("Region", refv("region_fr")),
                ],
            ),
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.boundary_data.equivalent_networks.len(), 1);
        let en = &network.cim.boundary_data.equivalent_networks[0];
        assert_eq!(en.mrid, "eqnet1");
        assert_eq!(en.name, "External_FR");
        assert_eq!(
            en.description.as_deref(),
            Some("French external equivalent")
        );
        assert_eq!(en.region_mrid.as_deref(), Some("region_fr"));
    }

    #[test]
    fn test_equivalent_branch_parsing() {
        let mut objects: ObjMap = HashMap::new();
        objects.insert(
            "eqbr1".to_string(),
            make_obj(
                "EquivalentBranch",
                &[
                    ("r", text("1.5")),
                    ("x", text("10.0")),
                    ("r0", text("3.0")),
                    ("x0", text("20.0")),
                    ("r21", text("1.5")),
                    ("x21", text("10.0")),
                    ("EquivalentNetwork", refv("eqnet1")),
                ],
            ),
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.boundary_data.equivalent_branches.len(), 1);
        let eb = &network.cim.boundary_data.equivalent_branches[0];
        assert_eq!(eb.mrid, "eqbr1");
        assert_eq!(eb.r_ohm, 1.5);
        assert_eq!(eb.x_ohm, 10.0);
        assert_eq!(eb.r0_ohm, Some(3.0));
        assert_eq!(eb.x0_ohm, Some(20.0));
        assert_eq!(eb.r2_ohm, Some(1.5));
        assert_eq!(eb.x2_ohm, Some(10.0));
        assert_eq!(eb.network_mrid.as_deref(), Some("eqnet1"));
        // No terminals → no bus resolution
        assert!(eb.from_bus.is_none());
        assert!(eb.to_bus.is_none());
    }

    #[test]
    fn test_equivalent_shunt_parsing() {
        let mut objects: ObjMap = HashMap::new();
        objects.insert(
            "eqsh1".to_string(),
            make_obj(
                "EquivalentShunt",
                &[
                    ("g", text("0.001")),
                    ("b", text("0.05")),
                    ("EquivalentNetwork", refv("eqnet1")),
                ],
            ),
        );

        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert_eq!(network.cim.boundary_data.equivalent_shunts.len(), 1);
        let es = &network.cim.boundary_data.equivalent_shunts[0];
        assert_eq!(es.mrid, "eqsh1");
        assert_eq!(es.g_s, 0.001);
        assert_eq!(es.b_s, 0.05);
        assert_eq!(es.network_mrid.as_deref(), Some("eqnet1"));
        assert!(es.bus.is_none());
    }

    #[test]
    fn test_empty_boundary_data_not_set() {
        let objects: ObjMap = HashMap::new();
        let idx = CgmesIndices::build(&objects);
        let mut network = Network::default();
        build_boundary_data(&objects, &idx, &mut network);

        assert!(network.cim.boundary_data.is_empty());
    }
}
