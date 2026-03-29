// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for `rebuild_topology` and `rebuild_topology_with_report`.

mod common;

use surge_network::Network;
use surge_network::network::topology::TerminalConnection;
use surge_network::network::{Branch, Bus, BusType, Generator, NodeBreakerTopology};
use surge_topology::{rebuild_topology, rebuild_topology_with_report};

use common::{make_grouped_mapping, make_identity_mapping, make_topology};

// ---------------------------------------------------------------------------
// rebuild_bus_merge_via_switch_close
// ---------------------------------------------------------------------------

#[test]
fn rebuild_bus_merge_via_switch_close() {
    let mut topo = make_topology(&["CN1", "CN2", "CN3"], &[("BRK_12", "CN1", "CN2", true)]);
    let mapping = make_identity_mapping(&["CN1", "CN2", "CN3"]);
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

    let mut stale = net;
    let changed = stale
        .topology
        .as_mut()
        .unwrap()
        .set_switch_state("BRK_12", false);
    assert!(changed, "switch state should have changed");

    let rebuilt = rebuild_topology_with_report(&stale).unwrap();

    assert!(
        !rebuilt.report.bus_merges.is_empty(),
        "expected at least one bus merge after closing the switch"
    );
    let merge = &rebuilt.report.bus_merges[0];
    assert!(
        merge.previous_bus_numbers.contains(&1) && merge.previous_bus_numbers.contains(&2),
        "expected buses 1 and 2 to be merged, got: {merge:?}"
    );
    assert!(
        rebuilt.network.buses.len() < stale.buses.len(),
        "expected fewer buses after merge: rebuilt={} vs original={}",
        rebuilt.network.buses.len(),
        stale.buses.len()
    );
    assert!(
        rebuilt
            .report
            .consumed_switch_ids
            .contains(&"BRK_12".to_string()),
        "BRK_12 should appear in consumed switches"
    );
}

// ---------------------------------------------------------------------------
// rebuild_idempotent
// ---------------------------------------------------------------------------

#[test]
fn rebuild_idempotent() {
    let mut topo = make_topology(
        &["CN_A", "CN_B", "CN_C"],
        &[
            ("BRK_1", "CN_A", "CN_B", false),
            ("BRK_2", "CN_B", "CN_C", true),
        ],
    );
    let mapping = make_grouped_mapping(&[&["CN_A", "CN_B"], &["CN_C"]]);
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

    let first = rebuild_topology(&net).unwrap();
    let second = rebuild_topology(&first).unwrap();

    assert_eq!(first.buses.len(), second.buses.len());
    assert_eq!(first.branches.len(), second.branches.len());

    let first_bus_nums: Vec<u32> = first.buses.iter().map(|b| b.number).collect();
    let second_bus_nums: Vec<u32> = second.buses.iter().map(|b| b.number).collect();
    assert_eq!(first_bus_nums, second_bus_nums);
}

// ---------------------------------------------------------------------------
// rebuild_preserves_generator_assignment
// ---------------------------------------------------------------------------

#[test]
fn rebuild_preserves_generator_assignment() {
    let mut topo = make_topology(
        &["CN_A", "CN_B", "CN_C"],
        &[("BRK_1", "CN_A", "CN_B", true)],
    );
    topo = NodeBreakerTopology::new(
        topo.substations.clone(),
        topo.voltage_levels.clone(),
        Vec::new(),
        topo.connectivity_nodes.clone(),
        Vec::new(),
        topo.switches.clone(),
        vec![TerminalConnection {
            terminal_id: "GEN_1_T1".into(),
            equipment_id: "GEN_1".into(),
            equipment_class: "SynchronousMachine".into(),
            sequence_number: 1,
            connectivity_node_id: "CN_A".into(),
        }],
    );

    let mapping = make_identity_mapping(&["CN_A", "CN_B", "CN_C"]);
    topo = topo.with_mapping(mapping);

    let mut generator = Generator::new(1, 100.0, 1.0);
    generator.machine_id = Some("GEN_1".into());

    let net = Network {
        buses: vec![
            Bus::new(1, BusType::Slack, 220.0),
            Bus::new(2, BusType::PQ, 220.0),
            Bus::new(3, BusType::PQ, 220.0),
        ],
        generators: vec![generator],
        branches: vec![
            Branch::new_line(1, 2, 0.01, 0.1, 0.02),
            Branch::new_line(2, 3, 0.01, 0.1, 0.02),
        ],
        topology: Some(topo),
        ..Default::default()
    };

    let rebuilt = rebuild_topology(&net).unwrap();

    let bus_numbers: Vec<u32> = rebuilt.buses.iter().map(|b| b.number).collect();
    for g in &rebuilt.generators {
        assert!(
            bus_numbers.contains(&g.bus),
            "generator (machine_id={:?}) references bus {} which does not exist in rebuilt network (buses: {bus_numbers:?})",
            g.machine_id,
            g.bus,
        );
    }

    let rebuilt_mapping = rebuilt
        .topology
        .as_ref()
        .expect("topology should be present after rebuild")
        .current_mapping()
        .expect("mapping should be current after rebuild");
    let expected_bus = rebuilt_mapping.connectivity_node_to_bus["CN_A"];
    assert_eq!(rebuilt.generators[0].bus, expected_bus);
}
