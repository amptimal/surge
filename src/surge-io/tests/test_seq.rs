// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Integration tests for PSS/E sequence data (.seq) parser.

use surge_io::psse::sequence::apply_text;
use surge_network::Network;
use surge_network::network::{Branch, Bus, BusType, Generator, Load, TransformerConnection};

/// Build a small 3-bus test network matching the embedded .seq test data.
fn make_test_network() -> Network {
    let mut net = Network::new("seq_test");
    net.base_mva = 100.0;
    net.buses = vec![
        {
            let mut b = Bus::new(1, BusType::Slack, 230.0);
            b.name = "Bus1".into();
            b.voltage_magnitude_pu = 1.04;
            b
        },
        {
            let mut b = Bus::new(2, BusType::PV, 230.0);
            b.name = "Bus2".into();
            b.voltage_magnitude_pu = 1.025;
            b
        },
        {
            let mut b = Bus::new(3, BusType::PQ, 230.0);
            b.name = "Bus3".into();
            b
        },
    ];
    net.loads = vec![Load::new(2, 21.7, 12.7), Load::new(3, 94.2, 19.0)];
    net.generators = vec![
        {
            let mut g = Generator::new(1, 71.6, 1.04);
            g.machine_id = Some("1".to_string());
            g.q = 27.0;
            g.machine_base_mva = 100.0;
            g.fault_data.get_or_insert_with(Default::default).xs = Some(0.15);
            g.pmax = 200.0;
            g
        },
        {
            let mut g = Generator::new(2, 163.0, 1.025);
            g.machine_id = Some("1".to_string());
            g.q = 6.7;
            g.machine_base_mva = 200.0;
            g.fault_data.get_or_insert_with(Default::default).xs = Some(0.20);
            g.pmax = 300.0;
            g
        },
    ];
    net.branches = vec![
        Branch::new_line(1, 2, 0.01, 0.085, 0.176),
        Branch::new_line(2, 3, 0.032, 0.161, 0.306),
        {
            let mut br = Branch::new_line(1, 3, 0.005, 0.10, 0.0);
            br.tap = 1.05;
            br.rating_a_mva = 300.0;
            br
        },
    ];
    net
}

/// Full .seq file with all sections populated.
const FULL_SEQ: &str = "\
! Machine section: I, ID, ZRPOS, ZXPOS, ZRNEG, ZXNEG, RZERO, XZERO, ZRGRND, ZXGRND
1, '1', 0.003, 0.15, 0.008, 0.17, 0.005, 0.12, 0.0, 0.1
2, '1', 0.004, 0.20, 0.010, 0.22, 0.006, 0.14, 0.02, 0.15
Q
! Branch section: I, J, CKT, RLINZ, XLINZ, BCHZ, GI, BI, GJ, BJ
1, 2, 1, 0.035, 0.28, 0.10, 0.0, 0.0, 0.0, 0.0
2, 3, 1, 0.10, 0.50, 0.15, 0.0, 0.0, 0.0, 0.0
Q
! Mutual section (empty)
Q
! 2W transformer section: I, J, CKT, CC, RG1, XG1, R01, X01, RG2, XG2, R02, X02
1, 3, 1, 2, 0.0, 0.0, 0.003, 0.08, 0.0, 0.0, 0.002, 0.05
Q
! Switched shunt (empty)
Q
! 3W transformer (empty)
Q
";

#[test]
fn test_seq_full_parse() {
    let mut net = make_test_network();
    let stats = apply_text(&mut net, FULL_SEQ).unwrap();

    assert_eq!(stats.machines_updated, 2, "expected 2 machines");
    assert_eq!(stats.branches_updated, 2, "expected 2 branches");
    assert_eq!(stats.transformers_updated, 1, "expected 1 transformer");
    assert_eq!(stats.mutual_couplings, 0, "expected 0 mutual couplings");
    assert_eq!(stats.skipped_records, 0, "expected 0 skipped records");
}

#[test]
fn test_seq_machine_data_applied() {
    let mut net = make_test_network();
    apply_text(&mut net, FULL_SEQ).unwrap();

    // Gen 1: bus=1, mbase=100, base_mva=100 → scale=1.0.
    let g1 = &net.generators[0];
    let fd1 = g1.fault_data.as_ref().expect("gen1 should have fault_data");
    assert!((fd1.x2_pu.unwrap() - 0.17).abs() < 1e-10, "gen1 x2_pu");
    assert!((fd1.r2_pu.unwrap() - 0.008).abs() < 1e-10, "gen1 r2_pu");
    assert!((fd1.x0_pu.unwrap() - 0.12).abs() < 1e-10, "gen1 x0_pu");
    assert!((fd1.r0_pu.unwrap() - 0.005).abs() < 1e-10, "gen1 r0_pu");
    let zn1 = fd1.zn.unwrap();
    assert!(zn1.re.abs() < 1e-10, "gen1 zn.re should be 0");
    assert!((zn1.im - 0.1).abs() < 1e-10, "gen1 zn.im = 0.1");

    // Gen 2: bus=2, mbase=200, base_mva=100 → scale=0.5.
    let g2 = &net.generators[1];
    let fd2 = g2.fault_data.as_ref().expect("gen2 should have fault_data");
    assert!((fd2.x2_pu.unwrap() - 0.22).abs() < 1e-10, "gen2 x2_pu");
    assert!((fd2.r2_pu.unwrap() - 0.010).abs() < 1e-10, "gen2 r2_pu");
    assert!((fd2.x0_pu.unwrap() - 0.14).abs() < 1e-10, "gen2 x0_pu");
    assert!((fd2.r0_pu.unwrap() - 0.006).abs() < 1e-10, "gen2 r0_pu");
    let zn2 = fd2.zn.unwrap();
    assert!((zn2.re - 0.01).abs() < 1e-10, "gen2 zn.re = 0.02*0.5");
    assert!((zn2.im - 0.075).abs() < 1e-10, "gen2 zn.im = 0.15*0.5");
}

#[test]
fn test_seq_branch_data_applied() {
    let mut net = make_test_network();
    apply_text(&mut net, FULL_SEQ).unwrap();

    // Branch 1→2: R0=0.035, X0=0.28, B0=0.10.
    let br1 = &net.branches[0];
    let zs1 = br1.zero_seq.as_ref().unwrap();
    assert!((zs1.r0 - 0.035).abs() < 1e-10);
    assert!((zs1.x0 - 0.28).abs() < 1e-10);
    assert!((zs1.b0 - 0.10).abs() < 1e-10);

    // Branch 2→3: R0=0.10, X0=0.50, B0=0.15.
    let br2 = &net.branches[1];
    let zs2 = br2.zero_seq.as_ref().unwrap();
    assert!((zs2.r0 - 0.10).abs() < 1e-10);
    assert!((zs2.x0 - 0.50).abs() < 1e-10);
    assert!((zs2.b0 - 0.15).abs() < 1e-10);
}

#[test]
fn test_seq_transformer_connection_applied() {
    let mut net = make_test_network();
    apply_text(&mut net, FULL_SEQ).unwrap();

    // Transformer 1→3: CC=2 → WyeGDelta.
    let br = &net.branches[2];
    assert_eq!(
        br.transformer_data.as_ref().unwrap().transformer_connection,
        TransformerConnection::WyeGDelta,
        "CC=2 should map to WyeGDelta"
    );
    // R0 = R01 + R02 = 0.003 + 0.002 = 0.005.
    assert!((br.zero_seq.as_ref().unwrap().r0 - 0.005).abs() < 1e-10);
    // X0 = X01 + X02 = 0.08 + 0.05 = 0.13.
    assert!((br.zero_seq.as_ref().unwrap().x0 - 0.13).abs() < 1e-10);
}

#[test]
fn test_seq_orphaned_records_skipped() {
    let mut net = make_test_network();
    let seq_data = "\
99, '1', 0.003, 0.15, 0.008, 0.17, 0.005, 0.12, 0.0, 0.1
Q
5, 6, 1, 0.04, 0.30, 0.10
Q
Q
Q
Q
Q
";
    let stats = apply_text(&mut net, seq_data).unwrap();
    assert_eq!(stats.machines_updated, 0);
    assert_eq!(stats.branches_updated, 0);
    assert_eq!(stats.skipped_records, 2);

    // Original data should be untouched.
    assert!(
        net.generators[0]
            .fault_data
            .as_ref()
            .and_then(|f| f.x0_pu)
            .is_none()
    );
    assert!(net.branches[0].zero_seq.is_none());
}

#[test]
fn test_seq_comments_and_blank_lines() {
    let mut net = make_test_network();
    let seq_data = "\
@! This is a full-line comment
! Another comment

1, '1', 0.003, 0.15, 0.008, 0.17, 0.005, 0.12, 0.0, 0.1  ! inline comment
Q
Q
Q
Q
Q
Q
";
    let stats = apply_text(&mut net, seq_data).unwrap();
    assert_eq!(stats.machines_updated, 1);
    assert!(
        (net.generators[0]
            .fault_data
            .as_ref()
            .unwrap()
            .x2_pu
            .unwrap()
            - 0.17)
            .abs()
            < 1e-10
    );
}
