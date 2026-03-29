// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0

mod common;

use surge_transfer::atc::AtcMargins;
use surge_transfer::types::{AtcOptions, NercAtcRequest, TransferPath};
use surge_transfer::{TransferStudy, compute_nerc_atc};

/// Helper: build a NercAtcRequest for a source→sink transfer on case30.
fn case30_request(margins: AtcMargins) -> NercAtcRequest {
    // Area 1 generators: buses 1, 2.  Area 2 loads: buses 12, 14, 15, 16, 17, 18, 19, 20.
    NercAtcRequest {
        path: TransferPath::new(
            "area1_to_area2",
            vec![1, 2],
            vec![12, 14, 15, 16, 17, 18, 19, 20],
        ),
        options: AtcOptions {
            monitored_branches: None, // all in-service rated branches
            contingency_branches: None,
            margins,
        },
    }
}

// ── (a) nerc_atc_case30_positive_ttc ─────────────────────────────────────────

#[test]
fn nerc_atc_case30_positive_ttc() {
    let net = common::load_case("case30");
    let request = case30_request(AtcMargins::default());

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    assert!(
        result.ttc_mw > 0.0,
        "TTC should be positive for a feasible transfer, got {}",
        result.ttc_mw,
    );
    assert!(
        result.atc_mw >= 0.0,
        "ATC must be non-negative, got {}",
        result.atc_mw,
    );
    assert!(
        result.atc_mw <= result.ttc_mw,
        "ATC ({}) must not exceed TTC ({})",
        result.atc_mw,
        result.ttc_mw,
    );
    assert!(
        result.binding_branch().is_some(),
        "binding_branch should be Some for a constrained transfer"
    );
}

// ── (b) nerc_atc_formula_holds ───────────────────────────────────────────────

#[test]
fn nerc_atc_formula_holds() {
    let net = common::load_case("case30");
    let request = case30_request(AtcMargins::default());

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    // NERC formula: ATC = max(0, TTC - TRM - CBM - ETC)
    let expected_atc = (result.ttc_mw - result.trm_mw - result.cbm_mw - result.etc_mw).max(0.0);
    let tol = 1e-9;

    assert!(
        (result.atc_mw - expected_atc).abs() < tol,
        "ATC formula mismatch: atc_mw={}, expected max(0, {} - {} - {} - {}) = {}",
        result.atc_mw,
        result.ttc_mw,
        result.trm_mw,
        result.cbm_mw,
        result.etc_mw,
        expected_atc,
    );
}

// ── (c) nerc_atc_custom_margins ──────────────────────────────────────────────

#[test]
fn nerc_atc_custom_margins() {
    let net = common::load_case("case30");
    let custom_margins = AtcMargins {
        trm_fraction: 0.10, // 10% of TTC
        cbm_mw: 50.0,
        etc_mw: 20.0,
    };
    let request = case30_request(custom_margins);

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    // Verify TRM is 10% of TTC.
    let expected_trm = result.ttc_mw * 0.10;
    let tol = 1e-9;
    assert!(
        (result.trm_mw - expected_trm).abs() < tol,
        "TRM should be 10% of TTC: expected {}, got {}",
        expected_trm,
        result.trm_mw,
    );

    // Verify CBM and ETC are reflected.
    assert!(
        (result.cbm_mw - 50.0).abs() < tol,
        "CBM should be 50 MW, got {}",
        result.cbm_mw,
    );
    assert!(
        (result.etc_mw - 20.0).abs() < tol,
        "ETC should be 20 MW, got {}",
        result.etc_mw,
    );

    // Verify ATC = max(0, TTC - TRM - CBM - ETC).
    let expected_atc = (result.ttc_mw - result.trm_mw - 50.0 - 20.0).max(0.0);
    assert!(
        (result.atc_mw - expected_atc).abs() < tol,
        "ATC with custom margins: expected {}, got {}",
        expected_atc,
        result.atc_mw,
    );
}

// ── (d) nerc_atc_etc_exceeds_ttc_gives_zero ──────────────────────────────────

#[test]
fn nerc_atc_etc_exceeds_ttc_gives_zero() {
    let net = common::load_case("case9");
    let huge_etc_margins = AtcMargins {
        trm_fraction: 0.0,
        cbm_mw: 0.0,
        etc_mw: 10_000.0, // far exceeds any possible TTC
    };
    // case9: source bus 1 (gen), sink bus 9 (load)
    let request = NercAtcRequest {
        path: TransferPath::new("bus1_to_bus9", vec![1], vec![9]),
        options: AtcOptions {
            monitored_branches: None,
            contingency_branches: None,
            margins: huge_etc_margins,
        },
    };

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    assert!(
        (result.atc_mw - 0.0).abs() < 1e-9,
        "ATC should be 0 when ETC (10000 MW) exceeds TTC ({}), got {}",
        result.ttc_mw,
        result.atc_mw,
    );
}

// ── (e) transfer_study_caches_base_state ─────────────────────────────────────

#[test]
fn transfer_study_caches_base_state() {
    let net = common::load_case("case30");
    let study = TransferStudy::new(&net).expect("TransferStudy creation should succeed");

    let request = case30_request(AtcMargins::default());

    let result1 = study
        .compute_nerc_atc(&request)
        .expect("first ATC call should succeed");
    let result2 = study
        .compute_nerc_atc(&request)
        .expect("second ATC call should succeed");

    assert!(
        (result1.atc_mw - result2.atc_mw).abs() < 1e-12,
        "ATC should be identical across calls: {} vs {}",
        result1.atc_mw,
        result2.atc_mw,
    );
    assert!(
        (result1.ttc_mw - result2.ttc_mw).abs() < 1e-12,
        "TTC should be identical across calls: {} vs {}",
        result1.ttc_mw,
        result2.ttc_mw,
    );
    assert_eq!(
        result1.limit_cause, result2.limit_cause,
        "limit cause should be identical across calls"
    );
    assert_eq!(
        result1.monitored_branches, result2.monitored_branches,
        "monitored_branches should be identical across calls"
    );
    assert_eq!(
        result1.transfer_ptdf.len(),
        result2.transfer_ptdf.len(),
        "transfer_ptdf length should be identical across calls"
    );
    for (i, (a, b)) in result1
        .transfer_ptdf
        .iter()
        .zip(result2.transfer_ptdf.iter())
        .enumerate()
    {
        assert!(
            (a - b).abs() < 1e-12,
            "transfer_ptdf[{i}] differs: {a} vs {b}"
        );
    }
}

// ── (f) nerc_atc_binding_branch_is_in_monitored_set ──────────────────────────

#[test]
fn nerc_atc_binding_branch_is_in_monitored_set() {
    let net = common::load_case("case30");
    let request = case30_request(AtcMargins::default());

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    if let Some(binding) = result.binding_branch() {
        assert!(
            result.monitored_branches.contains(&binding),
            "binding_branch {} must appear in monitored_branches {:?}",
            binding,
            result.monitored_branches,
        );
    }
}

// ── (g) nerc_atc_transfer_ptdf_nonzero ───────────────────────────────────────

#[test]
fn nerc_atc_transfer_ptdf_nonzero() {
    let net = common::load_case("case30");
    let request = case30_request(AtcMargins::default());

    let result = compute_nerc_atc(&net, &request).expect("ATC computation should succeed");

    assert!(
        !result.transfer_ptdf.is_empty(),
        "transfer_ptdf should not be empty"
    );

    let has_nonzero = result.transfer_ptdf.iter().any(|&p| p.abs() > 1e-12);
    assert!(
        has_nonzero,
        "at least one transfer PTDF entry should be nonzero — the transfer path must \
         affect some monitored branches. All PTDFs: {:?}",
        result.transfer_ptdf,
    );
}
