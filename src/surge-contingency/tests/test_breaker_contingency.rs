// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Tests for breaker-level contingency execution.
//!
//! Validates that contingencies with `switch_ids` are:
//! 1. Not filtered out by the OOS-branch filter
//! 2. Routed to the full-clone path (not LODF fast-path)
//! 3. Processed correctly: switches opened, network rebuilt, NR solved

use std::path::PathBuf;

use surge_contingency::{
    ContingencyOptions, ScreeningMode, analyze_contingencies, analyze_n1_branch,
};
use surge_network::network::topology::{
    BusbarSection, ConnectivityNode, Substation, TerminalConnection, VoltageLevel,
};
use surge_network::network::{Contingency, generate_breaker_contingencies};
use surge_network::network::{NodeBreakerTopology, SwitchDevice, SwitchType};

/// Return the path to a case file in the workspace-level examples/cases/ directory.
fn data_path(case_name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(format!(
        "../../examples/cases/{case_name}/{case_name}.surge.json.zst"
    ));
    p
}

/// Build a synthetic NodeBreakerTopology for case9.
///
/// Creates one substation with one voltage level.  Each bus gets a connectivity
/// node (CN_1 .. CN_9).  One closed breaker is placed between CN_1 and CN_4
/// (which maps to the branch between bus 1 and bus 4 in case9).
fn make_case9_substation_topology() -> NodeBreakerTopology {
    let cns: Vec<ConnectivityNode> = (1..=9)
        .map(|i| ConnectivityNode {
            id: format!("CN_{i}"),
            name: format!("CN_{i}"),
            voltage_level_id: "VL_345".into(),
        })
        .collect();

    let busbars: Vec<BusbarSection> = (1..=9)
        .map(|i| BusbarSection {
            id: format!("BB_{i}"),
            name: format!("Busbar {i}"),
            connectivity_node_id: format!("CN_{i}"),
            ip_max: None,
        })
        .collect();

    // Terminal connections: map each bus to its CN.
    // Equipment terminals: generators, loads, and branches reference buses.
    // We only need bus → CN mapping for rebuild_topology to remap equipment.
    let terminals: Vec<TerminalConnection> = (1..=9)
        .map(|i| TerminalConnection {
            terminal_id: format!("TERM_BUS_{i}"),
            equipment_id: format!("BUS_{i}"),
            equipment_class: "BusbarSection".into(),
            sequence_number: 1,
            connectivity_node_id: format!("CN_{i}"),
        })
        .collect();

    NodeBreakerTopology::new(
        vec![Substation {
            id: "SUB_1".into(),
            name: "Station 1".into(),
            region: None,
        }],
        vec![VoltageLevel {
            id: "VL_345".into(),
            name: "345 kV".into(),
            substation_id: "SUB_1".into(),
            base_kv: 345.0,
        }],
        vec![],
        cns,
        busbars,
        vec![SwitchDevice {
            id: "BRK_1_4".into(),
            name: "Breaker 1-4".into(),
            switch_type: SwitchType::Breaker,
            cn1_id: "CN_1".into(),
            cn2_id: "CN_4".into(),
            open: false, // closed
            normal_open: false,
            retained: false,
            rated_current: Some(2000.0),
        }],
        terminals,
    )
}

/// Load case9 and attach a synthetic NodeBreakerTopology.
fn load_case9_with_substation() -> surge_network::Network {
    let path = data_path("case9");
    if !path.exists() {
        panic!("case9.surge.json.zst not found at {path:?}");
    }
    let mut net = surge_io::load(&path).expect("parse case9");
    net.topology = Some(make_case9_substation_topology());
    net
}

// ---------------------------------------------------------------------------
// Test: switch-only contingency survives the OOS-branch filter
// ---------------------------------------------------------------------------

#[test]
fn test_breaker_ctg_not_filtered() {
    let net = load_case9_with_substation();

    let switch_ctg = Contingency {
        id: "breaker_test".into(),
        label: "Trip breaker 1-4".into(),
        switch_ids: vec!["BRK_1_4".into()],
        ..Default::default()
    };

    let opts = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let result = analyze_contingencies(&net, &[switch_ctg], &opts);
    assert!(result.is_ok(), "analyze_contingencies should succeed");
    let ca = result.unwrap();
    // The contingency should NOT be filtered out — it should appear in results.
    assert_eq!(
        ca.results.len(),
        1,
        "switch-only contingency must not be filtered out"
    );
}

// ---------------------------------------------------------------------------
// Test: modification-only contingency also not filtered (bonus fix)
// ---------------------------------------------------------------------------

#[test]
fn test_modification_only_ctg_not_filtered() {
    let net = load_case9_with_substation();

    let mod_ctg = Contingency {
        id: "mod_only".into(),
        label: "Modification only".into(),
        modifications: vec![surge_network::network::ContingencyModification::LoadSet {
            bus: 5,
            p_mw: 50.0,
            q_mvar: 10.0,
        }],
        ..Default::default()
    };

    let opts = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let result = analyze_contingencies(&net, &[mod_ctg], &opts);
    assert!(result.is_ok(), "analyze_contingencies should succeed");
    let ca = result.unwrap();
    assert_eq!(
        ca.results.len(),
        1,
        "modification-only contingency must not be filtered out"
    );
}

// ---------------------------------------------------------------------------
// Test: breaker contingency without NodeBreakerTopology is a no-op (converges as base case)
// ---------------------------------------------------------------------------

#[test]
fn test_breaker_ctg_no_substation_topology() {
    let path = data_path("case9");
    if !path.exists() {
        eprintln!("case9.surge.json.zst not found, skipping");
        return;
    }
    let net = surge_io::load(&path).expect("parse case9");
    assert!(net.topology.is_none());

    let switch_ctg = Contingency {
        id: "breaker_no_sm".into(),
        label: "Breaker without SM".into(),
        switch_ids: vec!["BRK_NONEXISTENT".into()],
        ..Default::default()
    };

    let opts = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let result = analyze_contingencies(&net, &[switch_ctg], &opts);
    assert!(result.is_ok());
    let ca = result.unwrap();
    assert_eq!(ca.results.len(), 1);
    // Without a NodeBreakerTopology, switch_ids have no effect — the network
    // solves as an unmodified base case (converges, no violations).
    assert!(
        ca.results[0].converged,
        "breaker ctg without NodeBreakerTopology should converge (no-op)"
    );
}

// ---------------------------------------------------------------------------
// Test: generate_breaker_contingencies produces correct output
// ---------------------------------------------------------------------------

#[test]
fn test_generate_breaker_contingencies() {
    let sm = make_case9_substation_topology();
    let ctgs = generate_breaker_contingencies(&sm);

    assert_eq!(ctgs.len(), 1, "one closed breaker → one contingency");
    assert_eq!(ctgs[0].switch_ids, vec!["BRK_1_4"]);
    assert!(ctgs[0].branch_indices.is_empty());
    assert!(ctgs[0].generator_indices.is_empty());
    assert_eq!(ctgs[0].id, "breaker_BRK_1_4");
}

// ---------------------------------------------------------------------------
// Test: include_breaker_contingencies option in analyze_n1_branch
// ---------------------------------------------------------------------------

#[test]
fn test_include_breaker_contingencies_option() {
    let net = load_case9_with_substation();
    let n_branches = net.branches.iter().filter(|b| b.in_service).count();

    // Without the option: only branch contingencies
    let opts_off = ContingencyOptions {
        screening: ScreeningMode::Off,
        include_breaker_contingencies: false,
        ..Default::default()
    };
    let ca_off = analyze_n1_branch(&net, &opts_off).expect("analyze_n1_branch without breakers");
    assert_eq!(
        ca_off.results.len(),
        n_branches,
        "without breaker flag, only branch contingencies"
    );

    // With the option: branch + breaker contingencies
    let opts_on = ContingencyOptions {
        screening: ScreeningMode::Off,
        include_breaker_contingencies: true,
        ..Default::default()
    };
    let ca_on = analyze_n1_branch(&net, &opts_on).expect("analyze_n1_branch with breakers");
    assert_eq!(
        ca_on.results.len(),
        n_branches + 1,
        "with breaker flag, should have branch + 1 breaker contingency"
    );

    // The breaker contingency result should exist
    let breaker_result = ca_on
        .results
        .iter()
        .find(|r| r.id.starts_with("breaker_"))
        .expect("should have a breaker contingency result");
    // It may or may not converge depending on topology, but it should exist
    eprintln!(
        "Breaker contingency '{}': converged={}",
        breaker_result.id, breaker_result.converged
    );
}

#[test]
fn test_breaker_contingency_survives_lodf_screening() {
    let net = load_case9_with_substation();
    let opts = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        include_breaker_contingencies: true,
        ..Default::default()
    };

    let analysis = analyze_n1_branch(&net, &opts).expect("analyze_n1_branch with LODF breakers");
    assert!(
        analysis
            .results
            .iter()
            .any(|result| result.id == "breaker_BRK_1_4"),
        "switch-only breaker contingencies must not be screened out by branch-only LODF logic"
    );
}

// ---------------------------------------------------------------------------
// Test: mixed branch + switch contingency applies both
// ---------------------------------------------------------------------------

#[test]
fn test_mixed_branch_and_switch_ctg() {
    let net = load_case9_with_substation();

    // Trip branch 0 AND open breaker BRK_1_4
    let mixed_ctg = Contingency {
        id: "mixed".into(),
        label: "Branch 0 + Breaker 1-4".into(),
        branch_indices: vec![0],
        switch_ids: vec!["BRK_1_4".into()],
        ..Default::default()
    };

    let opts = ContingencyOptions {
        screening: ScreeningMode::Off,
        ..Default::default()
    };

    let result = analyze_contingencies(&net, &[mixed_ctg], &opts);
    assert!(result.is_ok());
    let ca = result.unwrap();
    assert_eq!(ca.results.len(), 1);
    // The result exists — convergence depends on topology
    eprintln!("Mixed contingency: converged={}", ca.results[0].converged);
}
