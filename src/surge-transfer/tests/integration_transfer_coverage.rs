// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Extended integration tests for transfer capability surfaces.
//!
//! Covers AC-ATC, AFC, multi-transfer, injection capability, GSF/BLDF matrices,
//! and transfer-path validation error paths.

mod common;

use surge_transfer::atc::AtcMargins;
use surge_transfer::dfax::compute_afc;
use surge_transfer::injection::{InjectionCapabilityOptions, compute_injection_capability};
use surge_transfer::matrices::{compute_bldf, compute_gsf};
use surge_transfer::multi_transfer::compute_multi_transfer;
use surge_transfer::types::{
    AcAtcRequest, AfcRequest, AtcOptions, Flowgate, MultiTransferRequest, NercAtcRequest,
    TransferPath,
};
use surge_transfer::{TransferStudy, compute_ac_atc, compute_nerc_atc};

// ── helpers ─────────────────────────────────────────────────────────────────

fn case30_transfer_path() -> TransferPath {
    TransferPath::new(
        "area1_to_area2",
        vec![1, 2],
        vec![12, 14, 15, 16, 17, 18, 19, 20],
    )
}

// ── AC-ATC tests ────────────────────────────────────────────────────────────

#[test]
fn ac_atc_case30_returns_finite() {
    let net = common::load_case("case30");
    let request = AcAtcRequest::new(case30_transfer_path(), 0.90, 1.10);

    let result = compute_ac_atc(&net, &request).expect("AC-ATC should succeed on case30");

    assert!(
        result.atc_mw.is_finite() && result.atc_mw >= 0.0,
        "atc_mw should be finite and non-negative, got {}",
        result.atc_mw,
    );
    assert!(
        result.thermal_limit_mw.is_finite() && result.thermal_limit_mw > 0.0,
        "thermal_limit_mw should be finite and positive, got {}",
        result.thermal_limit_mw,
    );
    // voltage_limit_mw may be Infinity if voltage is never binding
    assert!(
        result.voltage_limit_mw > 0.0,
        "voltage_limit_mw should be positive, got {}",
        result.voltage_limit_mw,
    );
}

#[test]
fn ac_atc_thermal_le_nerc_ttc() {
    let net = common::load_case("case30");

    // NERC TTC with zero margins = raw thermal headroom
    let nerc_result = compute_nerc_atc(
        &net,
        &NercAtcRequest {
            path: case30_transfer_path(),
            options: AtcOptions {
                monitored_branches: None,
                contingency_branches: None,
                margins: AtcMargins {
                    trm_fraction: 0.0,
                    cbm_mw: 0.0,
                    etc_mw: 0.0,
                },
            },
        },
    )
    .expect("NERC ATC should succeed");

    let ac_result = compute_ac_atc(&net, &AcAtcRequest::new(case30_transfer_path(), 0.90, 1.10))
        .expect("AC-ATC should succeed");

    // AC-ATC adds voltage constraints, so it should be <= raw thermal TTC
    assert!(
        ac_result.atc_mw <= nerc_result.ttc_mw + 1e-6,
        "AC-ATC ({:.4} MW) should not exceed thermal TTC ({:.4} MW)",
        ac_result.atc_mw,
        nerc_result.ttc_mw,
    );
}

#[test]
fn ac_atc_binding_constraint_is_valid() {
    let net = common::load_case("case30");
    let request = AcAtcRequest::new(case30_transfer_path(), 0.90, 1.10);

    let result = compute_ac_atc(&net, &request).expect("AC-ATC should succeed");

    use surge_transfer::ac_atc::AcAtcLimitingConstraint;
    assert!(
        matches!(
            result.limiting_constraint,
            AcAtcLimitingConstraint::Thermal | AcAtcLimitingConstraint::Voltage
        ),
        "limiting_constraint should be Thermal or Voltage, got {:?}",
        result.limiting_constraint,
    );

    // If voltage-limited, limiting_bus should be set; if thermal, binding_branch
    match result.limiting_constraint {
        AcAtcLimitingConstraint::Voltage => {
            assert!(
                result.limiting_bus.is_some(),
                "voltage-limited result should have limiting_bus"
            );
        }
        AcAtcLimitingConstraint::Thermal => {
            assert!(
                result.binding_branch.is_some(),
                "thermal-limited result should have binding_branch"
            );
        }
    }
}

// ── AFC tests ───────────────────────────────────────────────────────────────

#[test]
fn afc_case30_flowgate_capacity() {
    let net = common::load_case("case30");
    let path = case30_transfer_path();

    // Pick the first in-service branch with a positive rating as the flowgate
    let fg_idx = net
        .branches
        .iter()
        .position(|b| b.in_service && b.rating_a_mva > 0.0)
        .expect("case30 should have rated branches");

    let request = AfcRequest {
        path,
        flowgates: vec![Flowgate::new(
            "fg1",
            fg_idx,
            None,
            net.branches[fg_idx].rating_a_mva,
            None,
        )],
    };

    let results = compute_afc(&net, &request).expect("AFC should succeed on case30");

    assert_eq!(results.len(), 1, "should get one AFC result per flowgate");
    let afc = &results[0];
    assert!(
        afc.afc_mw.is_finite(),
        "AFC should be finite, got {}",
        afc.afc_mw,
    );
    assert_eq!(afc.flowgate_name, "fg1");
}

#[test]
fn afc_zero_rating_rejects_invalid_flowgate() {
    let net = common::load_case("case30");
    let path = case30_transfer_path();

    let fg_idx = net
        .branches
        .iter()
        .position(|b| b.in_service)
        .expect("case30 should have in-service branches");

    let request = AfcRequest {
        path,
        flowgates: vec![Flowgate::new("fg_zero", fg_idx, None, 0.0, None)],
    };

    let result = compute_afc(&net, &request);
    assert!(
        result.is_err(),
        "zero-rated flowgate should be rejected as InvalidFlowgate"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("normal_rating_mw must be positive"),
        "unexpected error: {err}"
    );
}

// ── Multi-transfer tests ────────────────────────────────────────────────────

#[test]
fn multi_transfer_single_path_returns_result() {
    let net = common::load_case("case30");
    let path = case30_transfer_path();

    let request = MultiTransferRequest {
        paths: vec![path],
        weights: None,
        max_transfer_mw: None,
    };

    let result =
        compute_multi_transfer(&net, &request).expect("multi-transfer should succeed on case30");

    assert_eq!(
        result.transfer_mw.len(),
        1,
        "should get one transfer result per path"
    );
    assert!(
        result.transfer_mw[0].is_finite() && result.transfer_mw[0] >= 0.0,
        "transfer_mw should be finite and non-negative, got {}",
        result.transfer_mw[0],
    );
    assert!(
        result.total_weighted_transfer.is_finite(),
        "total_weighted_transfer should be finite"
    );
}

#[test]
fn multi_transfer_respects_max_transfer_cap() {
    let net = common::load_case("case30");
    let path = case30_transfer_path();
    let cap = 10.0; // small cap

    let request = MultiTransferRequest {
        paths: vec![path],
        weights: None,
        max_transfer_mw: Some(vec![cap]),
    };

    let result = compute_multi_transfer(&net, &request).expect("multi-transfer should succeed");
    assert!(
        result.transfer_mw[0] <= cap + 1e-6,
        "transfer_mw ({:.4}) should not exceed cap ({cap})",
        result.transfer_mw[0],
    );
}

// ── Injection capability tests ──────────────────────────────────────────────

#[test]
fn injection_capability_positive() {
    let net = common::load_case("case30");
    let options = InjectionCapabilityOptions::default();

    let result = compute_injection_capability(&net, &options)
        .expect("injection capability should succeed on case30");

    assert!(
        !result.by_bus.is_empty(),
        "injection capability should return at least one bus"
    );

    for &(bus, cap) in &result.by_bus {
        assert!(
            cap >= 0.0,
            "bus {bus} injection capability should be non-negative, got {cap}"
        );
    }
}

#[test]
fn injection_capability_covers_non_slack_buses() {
    let net = common::load_case("case30");
    let options = InjectionCapabilityOptions::default();

    let result =
        compute_injection_capability(&net, &options).expect("injection capability should succeed");

    use surge_network::network::BusType;
    let n_non_slack = net
        .buses
        .iter()
        .filter(|b| b.bus_type != BusType::Slack)
        .count();

    assert_eq!(
        result.by_bus.len(),
        n_non_slack,
        "should have one entry per non-slack bus"
    );
}

// ── TransferStudy tests ─────────────────────────────────────────────────────

#[test]
fn transfer_study_ac_atc_matches_standalone() {
    let net = common::load_case("case30");
    let study = TransferStudy::new(&net).expect("TransferStudy creation should succeed");
    let request = AcAtcRequest::new(case30_transfer_path(), 0.90, 1.10);

    let study_result = study
        .compute_ac_atc(&request)
        .expect("study AC-ATC should succeed");
    let standalone_result =
        compute_ac_atc(&net, &request).expect("standalone AC-ATC should succeed");

    let tol = 1e-6;
    assert!(
        (study_result.atc_mw - standalone_result.atc_mw).abs() < tol,
        "TransferStudy AC-ATC ({:.4}) should match standalone ({:.4})",
        study_result.atc_mw,
        standalone_result.atc_mw,
    );
}

// ── GSF / BLDF matrix tests ─────────────────────────────────────────────────

#[test]
fn gsf_dimensions_match() {
    let net = common::load_case("case30");
    let gsf = compute_gsf(&net).expect("GSF computation should succeed");

    let n_branches = net.n_branches();
    let n_gen = net.generators.iter().filter(|g| g.in_service).count();

    assert_eq!(
        gsf.values.nrows(),
        n_branches,
        "GSF rows should equal n_branches"
    );
    assert_eq!(
        gsf.values.ncols(),
        n_gen,
        "GSF cols should equal n_in_service_generators"
    );
    assert_eq!(gsf.branch_ids.len(), n_branches);
    assert_eq!(gsf.gen_buses.len(), n_gen);
}

#[test]
fn gsf_nonzero() {
    let net = common::load_case("case30");
    let gsf = compute_gsf(&net).expect("GSF computation should succeed");

    let has_nonzero = (0..gsf.values.nrows())
        .any(|r| (0..gsf.values.ncols()).any(|c| gsf.values[(r, c)].abs() > 1e-12));
    assert!(has_nonzero, "GSF matrix should contain nonzero entries");
}

#[test]
fn bldf_dimensions_match() {
    let net = common::load_case("case30");
    let bldf = compute_bldf(&net).expect("BLDF computation should succeed");

    assert_eq!(
        bldf.values.nrows(),
        net.n_buses(),
        "BLDF rows should equal n_buses"
    );
    assert_eq!(
        bldf.values.ncols(),
        net.n_branches(),
        "BLDF cols should equal n_branches"
    );
}

// ── Validation / error tests ────────────────────────────────────────────────

#[test]
fn empty_source_buses_errors() {
    let net = common::load_case("case9");
    let request = NercAtcRequest {
        path: TransferPath::new("bad_path", vec![], vec![9]),
        options: AtcOptions::default(),
    };

    let result = compute_nerc_atc(&net, &request);
    assert!(
        result.is_err(),
        "empty source_buses should produce an error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("source_buses must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn empty_sink_buses_errors() {
    let net = common::load_case("case9");
    let request = NercAtcRequest {
        path: TransferPath::new("bad_path", vec![1], vec![]),
        options: AtcOptions::default(),
    };

    let result = compute_nerc_atc(&net, &request);
    assert!(result.is_err(), "empty sink_buses should produce an error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sink_buses must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn empty_path_name_errors() {
    let net = common::load_case("case9");
    let request = NercAtcRequest {
        path: TransferPath::new("", vec![1], vec![9]),
        options: AtcOptions::default(),
    };

    let result = compute_nerc_atc(&net, &request);
    assert!(result.is_err(), "empty path name should produce an error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("path name must not be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn nonexistent_bus_errors() {
    let net = common::load_case("case9");
    let request = NercAtcRequest {
        path: TransferPath::new("ghost", vec![999], vec![9]),
        options: AtcOptions::default(),
    };

    let result = compute_nerc_atc(&net, &request);
    assert!(
        result.is_err(),
        "nonexistent source bus should produce an error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("999") && err.contains("not found"),
        "unexpected error: {err}"
    );
}
