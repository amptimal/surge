// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! CGMES Geographic Location (GL profile) builder.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::network::model::GeoPoint;

use super::types::ObjMap;

// ---------------------------------------------------------------------------
// Wave 29 — Geographic Location (GL profile)
// ---------------------------------------------------------------------------

/// Parse CGMES GL profile `Location` + `PositionPoint` objects into
/// `Network.cim.geo_locations`.
///
/// CGMES IEC 61970-301 GL profile:
/// - `Location.PowerSystemResource` -> reference from Location to equipment.
/// - `PositionPoint.Location` -> reference from point to its Location.
/// - `PositionPoint.sequenceNumber` (int): ordering within the Location.
/// - `PositionPoint.xPosition` (float): longitude (or easting in projected CRS).
/// - `PositionPoint.yPosition` (float): latitude (or northing).
/// - `PositionPoint.zPosition` (float, optional): elevation.
/// - `CoordinateSystem` (EQ): CRS reference (e.g., WGS84) -- stored as metadata only.
///
/// Each equipment mRID maps to an ordered list of `(x, y)` pairs.
/// Pure metadata -- does not affect power flow.
pub(crate) fn build_geo_locations(objects: &ObjMap, network: &mut Network) {
    // Build Location mRID -> equipment mRID map.
    let loc_to_eq: HashMap<String, String> = objects
        .iter()
        .filter(|(_, o)| o.class == "Location")
        .filter_map(|(loc_id, o)| {
            o.get_ref("PowerSystemResource")
                .map(|eq_id| (loc_id.clone(), eq_id.to_string()))
        })
        .collect();

    // Group PositionPoints by Location mRID, keeping (sequenceNumber, x, y).
    let mut points_by_loc: HashMap<String, Vec<(u32, f64, f64)>> = HashMap::new();
    for (_, pp_obj) in objects.iter().filter(|(_, o)| o.class == "PositionPoint") {
        let Some(loc_id) = pp_obj.get_ref("Location").map(|s| s.to_string()) else {
            continue;
        };
        let seq = pp_obj
            .get_text("sequenceNumber")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let x = pp_obj.parse_f64("xPosition").unwrap_or(0.0);
        let y = pp_obj.parse_f64("yPosition").unwrap_or(0.0);
        points_by_loc.entry(loc_id).or_default().push((seq, x, y));
    }

    // Sort each location's points by sequenceNumber and insert into network.
    for (loc_id, mut pts) in points_by_loc {
        let Some(eq_id) = loc_to_eq.get(&loc_id) else {
            continue;
        };
        pts.sort_by_key(|(seq, _, _)| *seq);
        let coords: Vec<GeoPoint> = pts.into_iter().map(|(_, x, y)| GeoPoint { x, y }).collect();
        network.cim.geo_locations.insert(eq_id.clone(), coords);
    }
}
