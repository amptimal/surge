// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! GO Competition Challenge 3 reserve-zone membership indexing.
//!
//! GO C3 problems declare active (real-power) and reactive reserve
//! zones separately. Each zone has a UID, a list of associated bus
//! UIDs, and (implicitly via bus→zone links) a list of devices that
//! participate in the zone's balance row.
//!
//! This module builds the inverse indexes the request builder needs:
//! zone UID → area ID, zone UID → participating bus UIDs, bus UID →
//! zone UIDs, and device UID → zone UIDs. The "primary" view is used
//! for resource/bus area assignments when neither active nor reactive
//! reserves are present on their own.

use std::collections::{BTreeMap, HashMap};

use surge_io::go_c3::GoC3Problem;

/// Membership indexes for a single zone kind (active or reactive).
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(super) struct ZoneMembership {
    pub zone_uid_to_area_id: HashMap<String, usize>,
    /// Ordered list of bus UIDs participating in each zone.
    pub zone_uid_to_bus_uids: BTreeMap<String, Vec<String>>,
    /// Reverse lookup: bus UID → zone UIDs it participates in.
    pub bus_zone_uids: HashMap<String, Vec<String>>,
    /// Device UID → zone UIDs it participates in (inherited from its bus).
    pub device_zone_uids: HashMap<String, Vec<String>>,
}

/// All three zone-membership views (primary, active, reactive).
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(super) struct ZoneAssignments {
    pub primary: ZoneMembership,
    pub active: ZoneMembership,
    pub reactive: ZoneMembership,
}

/// Build zone membership for a given zone-field selector.
fn build_membership(
    zones: &[surge_io::go_c3::types::GoC3ActiveZonalReserve],
    buses: &[surge_io::go_c3::types::GoC3Bus],
    devices: &[surge_io::go_c3::GoC3Device],
    select_zone_uids_active: bool,
) -> ZoneMembership {
    let zone_uid_to_area_id: HashMap<String, usize> = zones
        .iter()
        .enumerate()
        .map(|(idx, z)| (z.uid.clone(), idx + 1))
        .collect();

    let mut bus_zone_uids: HashMap<String, Vec<String>> = HashMap::new();
    let mut zone_uid_to_bus_uids: BTreeMap<String, Vec<String>> = zone_uid_to_area_id
        .keys()
        .map(|k| (k.clone(), Vec::new()))
        .collect();

    for bus in buses {
        let source = if select_zone_uids_active {
            &bus.active_reserve_uids
        } else {
            &bus.reactive_reserve_uids
        };
        let mut zones_for_bus: Vec<String> = source
            .iter()
            .filter(|uid| zone_uid_to_area_id.contains_key(uid.as_str()))
            .cloned()
            .collect();
        zones_for_bus.sort();
        zones_for_bus.dedup();
        if zones_for_bus.is_empty() {
            continue;
        }
        for zone_uid in &zones_for_bus {
            zone_uid_to_bus_uids
                .entry(zone_uid.clone())
                .or_default()
                .push(bus.uid.clone());
        }
        bus_zone_uids.insert(bus.uid.clone(), zones_for_bus);
    }

    for bus_uids in zone_uid_to_bus_uids.values_mut() {
        bus_uids.sort();
    }

    let mut device_zone_uids: HashMap<String, Vec<String>> = HashMap::new();
    for device in devices {
        if let Some(zone_uids) = bus_zone_uids.get(&device.bus) {
            device_zone_uids.insert(device.uid.clone(), zone_uids.clone());
        }
    }

    ZoneMembership {
        zone_uid_to_area_id,
        zone_uid_to_bus_uids,
        bus_zone_uids,
        device_zone_uids,
    }
}

/// Build zone membership for reactive zones.
fn build_membership_reactive(
    zones: &[surge_io::go_c3::types::GoC3ReactiveZonalReserve],
    buses: &[surge_io::go_c3::types::GoC3Bus],
    devices: &[surge_io::go_c3::GoC3Device],
) -> ZoneMembership {
    let zone_uid_to_area_id: HashMap<String, usize> = zones
        .iter()
        .enumerate()
        .map(|(idx, z)| (z.uid.clone(), idx + 1))
        .collect();

    let mut bus_zone_uids: HashMap<String, Vec<String>> = HashMap::new();
    let mut zone_uid_to_bus_uids: BTreeMap<String, Vec<String>> = zone_uid_to_area_id
        .keys()
        .map(|k| (k.clone(), Vec::new()))
        .collect();

    for bus in buses {
        let mut zones_for_bus: Vec<String> = bus
            .reactive_reserve_uids
            .iter()
            .filter(|uid| zone_uid_to_area_id.contains_key(uid.as_str()))
            .cloned()
            .collect();
        zones_for_bus.sort();
        zones_for_bus.dedup();
        if zones_for_bus.is_empty() {
            continue;
        }
        for zone_uid in &zones_for_bus {
            zone_uid_to_bus_uids
                .entry(zone_uid.clone())
                .or_default()
                .push(bus.uid.clone());
        }
        bus_zone_uids.insert(bus.uid.clone(), zones_for_bus);
    }

    for bus_uids in zone_uid_to_bus_uids.values_mut() {
        bus_uids.sort();
    }

    let mut device_zone_uids: HashMap<String, Vec<String>> = HashMap::new();
    for device in devices {
        if let Some(zone_uids) = bus_zone_uids.get(&device.bus) {
            device_zone_uids.insert(device.uid.clone(), zone_uids.clone());
        }
    }

    ZoneMembership {
        zone_uid_to_area_id,
        zone_uid_to_bus_uids,
        bus_zone_uids,
        device_zone_uids,
    }
}

/// Build the full `{primary, active, reactive}` zone-assignment set.
pub(super) fn build_zone_assignments(problem: &GoC3Problem) -> ZoneAssignments {
    let active = build_membership(
        &problem.network.active_zonal_reserve,
        &problem.network.bus,
        &problem.network.simple_dispatchable_device,
        true,
    );
    let reactive = build_membership_reactive(
        &problem.network.reactive_zonal_reserve,
        &problem.network.bus,
        &problem.network.simple_dispatchable_device,
    );
    let primary = if !problem.network.active_zonal_reserve.is_empty() {
        active.clone()
    } else {
        reactive.clone()
    };
    ZoneAssignments {
        primary,
        active,
        reactive,
    }
}
