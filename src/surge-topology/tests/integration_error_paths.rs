// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for topology report coverage (bus splits, collapsed
//! branches, edge cases).

mod common;

use surge_network::Network;
use surge_network::network::{Branch, Bus, BusType};
use surge_topology::{rebuild_topology, rebuild_topology_with_report};

use common::{make_grouped_mapping, make_identity_mapping, make_topology};

// ---------------------------------------------------------------------------
// Topology report: bus splits
// ---------------------------------------------------------------------------

#[test]
fn rebuild_bus_split_via_switch_open() {
    let mut topo = make_topology(&["CN_A", "CN_B"], &[("BRK_AB", "CN_A", "CN_B", false)]);
    let mapping = make_grouped_mapping(&[&["CN_A", "CN_B"]]);
    topo = topo.with_mapping(mapping);

    let net = Network {
        buses: vec![Bus::new(1, BusType::PQ, 220.0)],
        topology: Some(topo),
        ..Default::default()
    };

    let mut stale = net;
    let changed = stale
        .topology
        .as_mut()
        .unwrap()
        .set_switch_state("BRK_AB", true);
    assert!(changed);

    let rebuilt = rebuild_topology_with_report(&stale).unwrap();

    assert!(!rebuilt.report.bus_splits.is_empty());
    let split = &rebuilt.report.bus_splits[0];
    assert_eq!(split.previous_bus_number, 1);
    assert!(split.current_bus_numbers.len() >= 2);
    assert!(rebuilt.network.buses.len() > stale.buses.len());
}

// ---------------------------------------------------------------------------
// Topology report: collapsed branches
// ---------------------------------------------------------------------------

#[test]
fn rebuild_collapsed_branch_reported() {
    let mut topo = make_topology(&["CN_A", "CN_B"], &[("BRK_AB", "CN_A", "CN_B", true)]);
    let mapping = make_identity_mapping(&["CN_A", "CN_B"]);
    topo = topo.with_mapping(mapping);

    let net = Network {
        buses: vec![
            Bus::new(1, BusType::Slack, 220.0),
            Bus::new(2, BusType::PQ, 220.0),
        ],
        branches: vec![Branch::new_line(1, 2, 0.01, 0.1, 0.02)],
        topology: Some(topo),
        ..Default::default()
    };

    let mut stale = net;
    let changed = stale
        .topology
        .as_mut()
        .unwrap()
        .set_switch_state("BRK_AB", false);
    assert!(changed);

    let rebuilt = rebuild_topology_with_report(&stale).unwrap();
    assert!(!rebuilt.report.collapsed_branches.is_empty());
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn rebuild_all_switches_open() {
    let mut topo = make_topology(
        &["CN_A", "CN_B", "CN_C"],
        &[
            ("BRK_1", "CN_A", "CN_B", true),
            ("BRK_2", "CN_B", "CN_C", true),
        ],
    );
    let mapping = make_identity_mapping(&["CN_A", "CN_B", "CN_C"]);
    topo = topo.with_mapping(mapping);

    let net = Network {
        buses: vec![
            Bus::new(1, BusType::Slack, 220.0),
            Bus::new(2, BusType::PQ, 220.0),
            Bus::new(3, BusType::PQ, 220.0),
        ],
        branches: vec![
            Branch::new_line(1, 2, 0.01, 0.1, 0.02),
            Branch::new_line(2, 3, 0.01, 0.1, 0.02),
        ],
        topology: Some(topo),
        ..Default::default()
    };

    let rebuilt = rebuild_topology_with_report(&net).unwrap();
    assert_eq!(rebuilt.network.buses.len(), 3);
    assert!(rebuilt.report.bus_merges.is_empty());
}

#[test]
fn rebuild_single_bus_network() {
    let mut topo = make_topology(&["CN_ONLY"], &[]);
    let mapping = make_identity_mapping(&["CN_ONLY"]);
    topo = topo.with_mapping(mapping);

    let net = Network {
        buses: vec![Bus::new(1, BusType::Slack, 220.0)],
        topology: Some(topo),
        ..Default::default()
    };

    let rebuilt = rebuild_topology(&net).unwrap();
    assert_eq!(rebuilt.buses.len(), 1);
    assert_eq!(rebuilt.buses[0].number, 1);
}
