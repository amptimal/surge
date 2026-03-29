// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES area, region, and scheduled area transfer builder functions.

use std::collections::HashMap;

use surge_network::Network;

use super::indices::CgmesIndices;
use super::types::ObjMap;

pub(crate) fn build_area_schedules(objects: &ObjMap, network: &mut Network) {
    use surge_network::network::AreaSchedule;

    // Collect and sort by mRID for deterministic area numbering.
    let mut area_ids: Vec<&str> = objects
        .iter()
        .filter(|(_, o)| o.class == "ControlArea")
        .map(|(id, _)| id.as_str())
        .collect();
    area_ids.sort_unstable();

    for (area_num, area_id) in area_ids.iter().enumerate() {
        let obj = &objects[*area_id];
        let name = obj.get_text("name").unwrap_or(area_id).to_string();
        let p_desired_mw = obj.parse_f64("netInterchange").unwrap_or(0.0);
        let p_tolerance_mw = obj.parse_f64("pTolerance").unwrap_or(10.0);
        network.area_schedules.push(AreaSchedule {
            number: (area_num + 1) as u32,
            slack_bus: 0,
            p_desired_mw,
            p_tolerance_mw,
            name,
        });
        tracing::debug!(
            area_id,
            name = obj.get_text("name").unwrap_or(area_id),
            p_desired_mw,
            p_tolerance_mw,
            "CGMES ControlArea parsed (Wave 22)"
        );
    }
}

// ---------------------------------------------------------------------------
// Region from CGMES GeographicalRegion / SubGeographicalRegion
// ---------------------------------------------------------------------------

/// Populate `Network.metadata.regions` from CGMES `SubGeographicalRegion` (or fallback
/// `GeographicalRegion`) objects and assign `bus.zone` from the chain:
/// TN → ConnectivityNodeContainer(VL) → Substation → Region.
pub(crate) fn build_regions(objects: &ObjMap, idx: &CgmesIndices, network: &mut Network) {
    use surge_network::network::Region;

    // Collect SubGeographicalRegion objects (preferred — finer granularity).
    let mut sgr_ids: Vec<&str> = objects
        .iter()
        .filter(|(_, o)| o.class == "SubGeographicalRegion")
        .map(|(id, _)| id.as_str())
        .collect();
    sgr_ids.sort_unstable();

    let use_sgr = !sgr_ids.is_empty();

    // If no SubGeographicalRegion, fall back to GeographicalRegion.
    let region_ids: Vec<&str> = if use_sgr {
        sgr_ids
    } else {
        let mut gr_ids: Vec<&str> = objects
            .iter()
            .filter(|(_, o)| o.class == "GeographicalRegion")
            .map(|(id, _)| id.as_str())
            .collect();
        gr_ids.sort_unstable();
        gr_ids
    };

    if region_ids.is_empty() {
        return;
    }

    // mRID → sequential region number (1-based).
    let region_mrid_to_number: HashMap<&str, u32> = region_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, (i + 1) as u32))
        .collect();

    // Build Region records.
    for &rid in &region_ids {
        let obj = &objects[rid];
        let name = obj.get_text("name").unwrap_or(rid).to_string();
        let number = region_mrid_to_number[rid];
        network.metadata.regions.push(Region { number, name });
    }

    // Build Substation → region number mapping.
    // Chain: Substation.Region → SubGeographicalRegion (or GeographicalRegion).
    // If using SGR, Substation.Region points to SGR directly.
    // If using GR, Substation.Region may point to GR or SGR; try both.
    let substation_to_region: HashMap<&str, u32> = objects
        .iter()
        .filter(|(_, o)| o.class == "Substation")
        .filter_map(|(id, o)| {
            let region_ref = o.get_ref("Region")?;
            // Direct lookup in our region numbering.
            if let Some(&num) = region_mrid_to_number.get(region_ref) {
                return Some((id.as_str(), num));
            }
            // If using SGR, Substation.Region may point to a GR; look up the GR's
            // child SGR (take the first one that references this GR).
            if use_sgr {
                for &sgr_id in region_mrid_to_number.keys() {
                    if let Some(sgr_obj) = objects.get(sgr_id)
                        && sgr_obj.get_ref("Region") == Some(region_ref)
                    {
                        return Some((id.as_str(), region_mrid_to_number[sgr_id]));
                    }
                }
            }
            None
        })
        .collect();

    // Build VoltageLevel → Substation mapping.
    let vl_to_sub: HashMap<&str, &str> = objects
        .iter()
        .filter(|(_, o)| o.class == "VoltageLevel")
        .filter_map(|(id, o)| {
            let sub_ref = o
                .get_ref("Substation")
                .or_else(|| o.get_ref("MemberOf_Substation"))?;
            Some((id.as_str(), sub_ref))
        })
        .collect();

    // Assign bus.zone from the chain: TN → VL → Substation → Region.
    for (tn_id, &bus_num) in &idx.tn_bus {
        let tn_obj = match objects.get(tn_id.as_str()) {
            Some(o) => o,
            None => continue,
        };
        let vl_id = match tn_obj.get_ref("ConnectivityNodeContainer") {
            Some(v) => v,
            None => continue,
        };
        let sub_id = match vl_to_sub.get(vl_id) {
            Some(s) => *s,
            None => continue,
        };
        let region_num = match substation_to_region.get(sub_id) {
            Some(&n) => n,
            None => continue,
        };
        // Find the bus and set zone.
        if let Some(bus) = network.buses.iter_mut().find(|b| b.number == bus_num) {
            bus.zone = region_num;
        }
    }

    tracing::info!(
        regions = network.metadata.regions.len(),
        "CGMES regions built from {}",
        if use_sgr {
            "SubGeographicalRegion"
        } else {
            "GeographicalRegion"
        }
    );
}

// ---------------------------------------------------------------------------
// ScheduledAreaTransfer from CGMES TieFlow
// ---------------------------------------------------------------------------

/// Populate `Network.metadata.scheduled_area_transfers` from CGMES `TieFlow` objects.
///
/// TieFlow links a Terminal to a ControlArea, defining which branches are
/// tie-lines. The scheduled transfer amount comes from ControlArea.netInterchange
/// (already captured in AreaSchedule).
pub(crate) fn build_scheduled_area_transfers(
    objects: &ObjMap,
    idx: &CgmesIndices,
    network: &mut Network,
) {
    use surge_network::network::scheduled_area_transfer::ScheduledAreaTransfer;

    // Collect TieFlow objects.
    let tie_flows: Vec<&str> = objects
        .iter()
        .filter(|(_, o)| o.class == "TieFlow")
        .map(|(id, _)| id.as_str())
        .collect();

    if tie_flows.is_empty() {
        return;
    }

    // Build bus → area number mapping from existing area_schedules + bus.area.
    let bus_area: HashMap<u32, u32> = network
        .buses
        .iter()
        .filter(|b| b.area > 0)
        .map(|b| (b.number, b.area))
        .collect();

    // Build ControlArea mRID → area number mapping.
    let mut ca_ids: Vec<&str> = objects
        .iter()
        .filter(|(_, o)| o.class == "ControlArea")
        .map(|(id, _)| id.as_str())
        .collect();
    ca_ids.sort_unstable();
    let ca_to_num: HashMap<&str, u32> = ca_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, (i + 1) as u32))
        .collect();

    let mut transfer_id = 0u32;
    let mut seen_pairs: HashMap<(u32, u32), u32> = HashMap::new();

    for &tf_id in &tie_flows {
        let tf = &objects[tf_id];
        let term_ref = match tf.get_ref("Terminal") {
            Some(t) => t,
            None => continue,
        };
        let ca_ref = match tf.get_ref("ControlArea") {
            Some(c) => c,
            None => continue,
        };
        let _ca_num = match ca_to_num.get(ca_ref) {
            Some(&n) => n,
            None => continue,
        };

        // Resolve Terminal → ConductingEquipment → both terminals → buses.
        let term_obj = match objects.get(term_ref) {
            Some(o) => o,
            None => continue,
        };
        let eq_id = match term_obj.get_ref("ConductingEquipment") {
            Some(e) => e,
            None => continue,
        };
        let terminals = idx.terminals(eq_id);
        if terminals.len() < 2 {
            continue;
        }
        // Resolve both terminal buses.
        let bus1 = terminals.iter().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let bus2 = terminals.iter().rev().find_map(|tid| {
            let tn = idx.terminal_tn(objects, tid)?;
            idx.tn_bus(tn)
        });
        let (from_bus, to_bus) = match (bus1, bus2) {
            (Some(b1), Some(b2)) if b1 != b2 => (b1, b2),
            _ => continue,
        };

        // Determine areas from buses.
        let from_area = bus_area.get(&from_bus).copied().unwrap_or(0);
        let to_area = bus_area.get(&to_bus).copied().unwrap_or(0);
        if from_area == 0 || to_area == 0 || from_area == to_area {
            continue; // Not an inter-area tie or areas unknown.
        }

        // Normalize direction so from_area < to_area for dedup.
        let (fa, ta) = if from_area < to_area {
            (from_area, to_area)
        } else {
            (to_area, from_area)
        };

        let pair_count = seen_pairs.entry((fa, ta)).or_insert(0);
        *pair_count += 1;
        transfer_id += 1;

        network
            .metadata
            .scheduled_area_transfers
            .push(ScheduledAreaTransfer {
                from_area: fa,
                to_area: ta,
                id: transfer_id,
                p_transfer_mw: 0.0, // TieFlow defines topology, not transfer amount.
            });
    }

    if !network.metadata.scheduled_area_transfers.is_empty() {
        tracing::info!(
            transfers = network.metadata.scheduled_area_transfers.len(),
            "CGMES TieFlow → scheduled area transfers"
        );
    }
}
