// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::collections::HashMap;

use super::types::{CimObj, CimVal, ObjMap};
use crate::union_find::UnionFindStr;

// ---------------------------------------------------------------------------
// Stage 1b — Bus-breaker topology reduction (Union-Find)
// ---------------------------------------------------------------------------

pub(crate) fn has_topological_nodes(objects: &ObjMap) -> bool {
    objects.values().any(|o| o.class == "TopologicalNode")
}

/// Switch CIM classes we recognise.
pub(crate) const SWITCH_CLASSES: &[&str] = &[
    "Switch",
    "Breaker",
    "Disconnector",
    "LoadBreakSwitch",
    "GroundDisconnector",
    "Fuse",
];

/// If the model only has ConnectivityNodes (bus-breaker), merge them through
/// closed switches into synthetic TopologicalNodes, then rewrite terminal refs.
pub(crate) fn reduce_topology(objects: &mut ObjMap) {
    if has_topological_nodes(objects) {
        return;
    }

    let cn_keys: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "ConnectivityNode")
        .map(|(k, _)| k.clone())
        .collect();
    if cn_keys.is_empty() {
        return;
    }

    let switch_classes = [
        "Switch",
        "Breaker",
        "Disconnector",
        "LoadBreakSwitch",
        "GroundDisconnector",
        "Fuse",
    ];

    // terminal_id → CN_id
    let terminal_to_cn: HashMap<String, String> = objects
        .iter()
        .filter(|(_, o)| o.class == "Terminal")
        .filter_map(|(id, o)| {
            o.get_ref("ConnectivityNode")
                .map(|cn| (id.clone(), cn.to_string()))
        })
        .collect();

    // equipment_id → [terminal_ids] (for switches)
    let switch_terminals: HashMap<String, Vec<String>> = {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (tid, t) in objects.iter().filter(|(_, o)| o.class == "Terminal") {
            if let Some(eq_id) = t.get_ref("ConductingEquipment")
                && let Some(o) = objects.get(eq_id)
                && switch_classes.contains(&o.class.as_str())
            {
                m.entry(eq_id.to_string()).or_default().push(tid.clone());
            }
        }
        m
    };

    let mut uf = UnionFindStr::new();
    for k in &cn_keys {
        uf.ensure(k);
    }

    // Closed switches collapse their two CNs into one TopologicalNode (union-find).
    // Open switches are intentionally NOT added as branches: they represent open circuits
    // (zero admittance), which is correctly modeled by leaving the two CNs separate.
    // SSH `open` attribute takes precedence over EQ `normalOpen` (operational vs design).
    for (sw_id, sw_terms) in &switch_terminals {
        let sw = &objects[sw_id];
        let open = match sw.get_text("open").or_else(|| sw.get_text("normalOpen")) {
            Some(s) if s.eq_ignore_ascii_case("true") || s == "1" => true,
            Some(s) if s.eq_ignore_ascii_case("false") || s == "0" => false,
            Some(s) => {
                tracing::warn!(
                    sw_id,
                    raw = s,
                    "CGMES switch state is malformed; leaving switch open instead of closing it"
                );
                true
            }
            None => true,
        };
        if !open && sw_terms.len() >= 2 {
            let cn1 = terminal_to_cn
                .get(&sw_terms[0])
                .map(|s| s.as_str())
                .unwrap_or(&sw_terms[0]);
            let cn2 = terminal_to_cn
                .get(&sw_terms[1])
                .map(|s| s.as_str())
                .unwrap_or(&sw_terms[1]);
            uf.union(cn1, cn2);
        }
    }

    let cn_container: HashMap<String, String> = objects
        .iter()
        .filter(|(_, o)| o.class == "ConnectivityNode")
        .filter_map(|(id, o)| {
            o.get_ref("ConnectivityNodeContainer")
                .map(|container| (id.clone(), container.to_string()))
        })
        .collect();

    let vl_base_voltage: HashMap<String, String> = objects
        .iter()
        .filter(|(_, o)| o.class == "VoltageLevel")
        .filter_map(|(id, o)| {
            o.get_ref("BaseVoltage")
                .map(|bv| (id.clone(), bv.to_string()))
        })
        .collect();

    // Assign TN mRIDs to equivalence classes.
    let mut tn_map: HashMap<String, String> = HashMap::new();
    let mut tn_roots: Vec<(String, String)> = Vec::new();
    for cn_id in &cn_keys {
        let root = uf.find(cn_id);
        if !tn_map.contains_key(&root) {
            let tn_id = format!("TN_{root}");
            tn_map.insert(root.clone(), tn_id.clone());
            tn_roots.push((root, tn_id));
        }
    }

    // Insert synthetic TopologicalNode objects with enough metadata to preserve
    // base-voltage resolution and deterministic naming in pure node-breaker files.
    for (root, tn_id) in tn_roots {
        let cn_obj = match objects.get(root.as_str()) {
            Some(obj) => obj,
            None => {
                objects
                    .entry(tn_id)
                    .or_insert_with(|| CimObj::new("TopologicalNode"));
                continue;
            }
        };

        let mut tn = CimObj::new("TopologicalNode");
        if let Some(name) = cn_obj.get_text("name")
            && !name.is_empty()
        {
            tn.attrs
                .insert("name".to_string(), CimVal::Text(name.to_string()));
        }
        if let Some(vl_id) = cn_container.get(root.as_str()) {
            tn.attrs.insert(
                "ConnectivityNodeContainer".to_string(),
                CimVal::Ref(vl_id.clone()),
            );
            if let Some(bv_id) = vl_base_voltage.get(vl_id) {
                tn.attrs
                    .insert("BaseVoltage".to_string(), CimVal::Ref(bv_id.clone()));
            }
        }
        objects.entry(tn_id).or_insert(tn);
    }

    // Rewrite Terminal.ConnectivityNode → Terminal.TopologicalNode
    let terminal_ids: Vec<String> = objects
        .iter()
        .filter(|(_, o)| o.class == "Terminal")
        .map(|(k, _)| k.clone())
        .collect();

    for tid in terminal_ids {
        let cn_id = objects[&tid]
            .get_ref("ConnectivityNode")
            .map(|s| s.to_string());
        if let Some(cn_id) = cn_id {
            let root = uf.find(&cn_id);
            if let Some(tn_id) = tn_map.get(&root).cloned()
                && let Some(t) = objects.get_mut(&tid)
            {
                t.attrs
                    .insert("TopologicalNode".to_string(), CimVal::Ref(tn_id));
            }
        }
    }
}
