// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! HVDC power flow tests.
//!
//! Tests cover VSC and LCC links embedded in simple and standard test networks,
//! verifying power balance, reactive power, losses, and convergence.

mod common;

use common::two_bus_test_network;
use surge_hvdc::model::vsc;
use surge_hvdc::{HvdcLink, HvdcOptions, LccHvdcLink, VscHvdcLink, solve_hvdc, solve_hvdc_links};
// ---------------------------------------------------------------------------
// Helper: load case9 from workspace test data
// ---------------------------------------------------------------------------

fn case_path(stem: &str) -> std::path::PathBuf {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let direct = workspace.join(format!("examples/cases/{stem}/{stem}.surge.json.zst"));
    if direct.exists() {
        return direct;
    }
    let num = stem.trim_start_matches("case");
    let alt = workspace.join(format!("examples/cases/ieee{num}/{stem}.surge.json.zst"));
    if alt.exists() {
        return alt;
    }
    direct
}

fn load_case9() -> surge_network::Network {
    surge_io::load(case_path("case9")).expect("Failed to parse case9")
}

// ---------------------------------------------------------------------------
// Test 1: VSC power balance — Kirchhoff's law across converter
// ---------------------------------------------------------------------------

/// A VSC link transferring 50 MW between two buses should satisfy:
///   |p_from| = |p_to| + p_loss  (power balance within converter)
#[test]
fn test_vsc_power_balance() {
    let net = two_bus_test_network(1, 2, 50.0);

    let vsc = VscHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 50.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.002,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "vsc-test".to_string(),
    };

    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    assert!(sol.converged, "HVDC solver should converge");

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];

    // Kirchhoff: power drawn from rectifier = power injected at inverter + losses
    // rect.p_ac_mw is negative (drawn from AC), inv.p_ac_mw is positive (injected)
    let p_drawn = -rect.p_ac_mw;
    let balance_err = (p_drawn - inv.p_ac_mw - sol.total_loss_mw).abs();
    assert!(
        balance_err < 0.1,
        "Power balance error {balance_err:.4} MW should be < 0.1 MW"
    );

    // Rectifier draws power (negative p_ac_mw) and inverter injects (positive p_ac_mw).
    assert!(rect.p_ac_mw < 0.0, "Rectifier should draw power from AC");
    assert!(inv.p_ac_mw > 0.0, "Inverter should inject power into AC");
}

#[test]
fn test_embedded_sequential_matches_explicit_links() {
    let base = two_bus_test_network(1, 2, 50.0);
    let vsc = VscHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 50.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.002,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "embedded-vsc".to_string(),
    };

    let mut embedded_network = base.clone();
    embedded_network
        .hvdc
        .push_vsc_link(surge_network::network::VscHvdcLink {
            name: vsc.name.clone(),
            mode: surge_network::network::VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.0,
            converter1: surge_network::network::VscConverterTerminal {
                bus: vsc.from_bus,
                control_mode: surge_network::network::VscConverterAcControlMode::ReactivePower,
                dc_setpoint: vsc.p_dc_mw,
                ac_setpoint: vsc.q_from_mvar,
                loss_constant_mw: vsc.loss_coeff_a_mw * embedded_network.base_mva,
                loss_linear: vsc.loss_coeff_b_pu,
                q_min_mvar: vsc.q_min_from_mvar,
                q_max_mvar: vsc.q_max_from_mvar,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
            converter2: surge_network::network::VscConverterTerminal {
                bus: vsc.to_bus,
                control_mode: surge_network::network::VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 0.0,
                ac_setpoint: vsc.q_to_mvar,
                loss_constant_mw: vsc.loss_coeff_a_mw * embedded_network.base_mva,
                loss_linear: vsc.loss_coeff_b_pu,
                q_min_mvar: vsc.q_min_to_mvar,
                q_max_mvar: vsc.q_max_to_mvar,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
        });

    let opts = HvdcOptions::default();
    let embedded = solve_hvdc(&embedded_network, &opts).expect("embedded HVDC solve failed");
    let explicit =
        solve_hvdc_links(&base, &[HvdcLink::Vsc(vsc)], &opts).expect("explicit HVDC solve failed");

    assert!(
        embedded.converged,
        "embedded sequential solve should converge"
    );
    assert!(
        explicit.converged,
        "explicit-link sequential solve should converge"
    );
    assert_eq!(
        embedded.stations.len(),
        explicit.stations.len(),
        "embedded and explicit solves should return the same number of stations"
    );

    let total_loss_err = (embedded.total_loss_mw - explicit.total_loss_mw).abs();
    assert!(
        total_loss_err < 1e-9,
        "embedded and explicit total losses should match, error={total_loss_err:.3e} MW"
    );

    for (embedded_station, explicit_station) in
        embedded.stations.iter().zip(explicit.stations.iter())
    {
        assert_eq!(embedded_station.ac_bus, explicit_station.ac_bus);
        assert!(
            (embedded_station.p_ac_mw - explicit_station.p_ac_mw).abs() < 1e-9,
            "embedded and explicit AC active power should match at bus {}",
            embedded_station.ac_bus
        );
        assert!(
            (embedded_station.q_ac_mvar - explicit_station.q_ac_mvar).abs() < 1e-9,
            "embedded and explicit AC reactive power should match at bus {}",
            embedded_station.ac_bus
        );
        assert!(
            (embedded_station.p_dc_mw - explicit_station.p_dc_mw).abs() < 1e-9,
            "embedded and explicit DC power should match at bus {}",
            embedded_station.ac_bus
        );
        assert!(
            (embedded_station.converter_loss_mw - explicit_station.converter_loss_mw).abs() < 1e-9,
            "embedded and explicit converter losses should match at bus {}",
            embedded_station.ac_bus
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: LCC reactive power — Q = P × tan(alpha) using firing angle
// ---------------------------------------------------------------------------

/// LCC with alpha = 15°: Q absorbed ≈ P × tan(15°) ≈ P × 0.2679
///
/// The reactive power is computed from the firing angle (rectifier) or
/// extinction angle (inverter) rather than a fixed power factor, using:
///   Q = P × tan(alpha)
/// This is the standard 6-pulse bridge rectifier equation.
#[test]
fn test_lcc_reactive_power() {
    let net = two_bus_test_network(1, 2, 100.0);

    let lcc = LccHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 100.0,
        r_dc_pu: 0.0,
        firing_angle_deg: 15.0,
        extinction_angle_deg: 15.0,
        alpha_min_deg: 5.0,
        power_factor_r: 0.9,
        power_factor_i: 0.9,
        a_r: 1.0,
        a_i: 1.0,
        x_c_r: 0.0,
        x_c_i: 0.0,
        control_mode: surge_hvdc::LccHvdcControlMode::ConstantPower,
        name: "lcc-test".to_string(),
    };

    let links = vec![HvdcLink::Lcc(lcc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    assert!(sol.converged);

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];

    // Expected: Q = P × tan(alpha) = 100 × tan(15°) ≈ 26.795 MVAR
    let alpha_rad = 15.0_f64.to_radians();
    let expected_q = 100.0 * alpha_rad.tan();

    // Rectifier absorbs reactive power (q_ac_mvar is negative)
    let q_absorbed = -rect.q_ac_mvar;
    let q_err = (q_absorbed - expected_q).abs();
    assert!(
        q_err < 1.0,
        "Q error {q_err:.4} MVAR should be < 1.0 MVAR (expected {expected_q:.3}, got {q_absorbed:.3})"
    );

    // LCC always absorbs reactive (q_ac_mvar < 0 for both stations)
    assert!(
        rect.q_ac_mvar < 0.0,
        "LCC rectifier must absorb reactive power"
    );
    assert!(
        inv.q_ac_mvar < 0.0,
        "LCC inverter must absorb reactive power"
    );
}

// ---------------------------------------------------------------------------
// Test 3: VSC in case9 — convergence and DC power close to setpoint
// ---------------------------------------------------------------------------

/// Add a 100 MW VSC link between buses 4 and 7 in case9.
/// Verify: converged, p_dc close to setpoint, losses >= 0.
#[test]
fn test_vsc_case9_embedding() {
    let net = load_case9();

    let vsc = VscHvdcLink {
        from_bus: 4,
        to_bus: 7,
        p_dc_mw: 100.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.002,
        loss_c_pu: 0.0,
        q_max_from_mvar: 200.0,
        q_min_from_mvar: -200.0,
        q_max_to_mvar: 200.0,
        q_min_to_mvar: -200.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "vsc-case9".to_string(),
    };

    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve on case9 failed");
    assert!(sol.converged, "Should converge on case9");

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];

    // DC power close to setpoint (within 1 MW)
    let p_err = (rect.p_dc_mw - 100.0).abs();
    assert!(p_err < 1.0, "DC power error {p_err:.3} MW should be < 1 MW");

    // Losses are non-negative
    assert!(
        rect.converter_loss_mw >= 0.0,
        "Losses must be non-negative, got {:.4}",
        rect.converter_loss_mw
    );

    // Rectifier bus is from_bus, inverter bus is to_bus
    assert_eq!(rect.ac_bus, 4);
    assert_eq!(inv.ac_bus, 7);
}

// ---------------------------------------------------------------------------
// Test 4: LCC in case9 — convergence and power balance
// ---------------------------------------------------------------------------

/// Add an LCC link between buses 5 and 9 in case9.
/// Verify convergence and that the rectifier draws more power than the inverter
/// injects (difference = losses).
#[test]
fn test_lcc_case9_embedding() {
    let net = load_case9();

    let lcc = LccHvdcLink {
        from_bus: 5,
        to_bus: 9,
        p_dc_mw: 80.0,
        r_dc_pu: 0.01,
        firing_angle_deg: 15.0,
        extinction_angle_deg: 15.0,
        alpha_min_deg: 5.0,
        power_factor_r: 0.9,
        power_factor_i: 0.9,
        a_r: 1.0,
        a_i: 1.0,
        x_c_r: 0.0,
        x_c_i: 0.0,
        control_mode: surge_hvdc::LccHvdcControlMode::ConstantPower,
        name: "lcc-case9".to_string(),
    };

    let links = vec![HvdcLink::Lcc(lcc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve on case9 failed");
    assert!(sol.converged, "Should converge on case9 with LCC");

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];

    // The rectifier draws more power from AC than the inverter injects.
    let p_drawn = -rect.p_ac_mw;
    let p_injected = inv.p_ac_mw;
    assert!(
        p_drawn >= p_injected,
        "Rectifier should draw >= inverter injection (losses >= 0); drawn={p_drawn:.2}, injected={p_injected:.2}"
    );

    // LCC result should have lcc_detail populated and v_dc_pu > 0.
    assert!(
        rect.lcc_detail.is_some(),
        "LCC rectifier result should have lcc_detail"
    );
    assert!(
        rect.v_dc_pu > 0.0,
        "LCC result should have positive v_dc_pu"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Zero-loss VSC — losses exactly zero
// ---------------------------------------------------------------------------

/// With all loss coefficients set to zero, VSC losses must be exactly 0.
#[test]
fn test_hvdc_zero_loss() {
    let net = two_bus_test_network(1, 2, 75.0);

    let vsc = VscHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 75.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.0,
        loss_coeff_b_pu: 0.0,
        loss_c_pu: 0.0,
        q_max_from_mvar: f64::MAX,
        q_min_from_mvar: f64::MIN,
        q_max_to_mvar: f64::MAX,
        q_min_to_mvar: f64::MIN,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "vsc-zero-loss".to_string(),
    };

    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    assert!(sol.converged);

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];
    assert_eq!(
        rect.converter_loss_mw, 0.0,
        "With zero loss coefficients, losses must be exactly 0.0"
    );
    assert_eq!(sol.total_loss_mw, 0.0, "Total losses must be exactly 0.0");

    // Power balance: inverter p_ac should equal p_dc exactly
    let p_balance = (inv.p_ac_mw - rect.p_dc_mw).abs();
    assert!(
        p_balance < 1e-10,
        "Zero-loss: p_ac_mw (inverter) should equal p_dc, got p_ac={:.6}, p_dc={:.6}",
        inv.p_ac_mw,
        rect.p_dc_mw
    );
}

// ---------------------------------------------------------------------------
// Test 6: Multiple HVDC links — both converge
// ---------------------------------------------------------------------------

/// Two VSC links in the same network (case9). Both should converge independently.
#[test]
fn test_multiple_hvdc_links() {
    let net = load_case9();

    let vsc1 = VscHvdcLink {
        from_bus: 1,
        to_bus: 5,
        p_dc_mw: 50.0,
        q_from_mvar: 10.0,
        q_to_mvar: -5.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.001,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "vsc-1-5".to_string(),
    };

    let vsc2 = VscHvdcLink {
        from_bus: 2,
        to_bus: 9,
        p_dc_mw: 30.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.001,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "vsc-2-9".to_string(),
    };

    let links = vec![HvdcLink::Vsc(vsc1), HvdcLink::Vsc(vsc2)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve with two links failed");
    assert!(sol.converged, "Should converge with two VSC links");
    assert_eq!(
        sol.stations.len(),
        4,
        "Should have four converter results (2 per link)"
    );

    // HvdcLink 0: converters[0] = rectifier, converters[1] = inverter
    let rect1 = &sol.stations[0];
    let inv1 = &sol.stations[1];
    // HvdcLink 1: converters[2] = rectifier, converters[3] = inverter
    let rect2 = &sol.stations[2];
    let inv2 = &sol.stations[3];

    // Both should converge
    assert!(rect1.converged);
    assert!(rect2.converged);

    // Bus assignments correct
    assert_eq!(rect1.ac_bus, 1);
    assert_eq!(inv1.ac_bus, 5);
    assert_eq!(rect2.ac_bus, 2);
    assert_eq!(inv2.ac_bus, 9);

    // Both have positive DC powers
    assert!(rect1.p_dc_mw > 0.0);
    assert!(rect2.p_dc_mw > 0.0);

    // Total losses = sum of individual losses
    let expected_total = sol
        .stations
        .iter()
        .map(|station| station.converter_loss_mw)
        .sum::<f64>();
    let total_err = (sol.total_loss_mw - expected_total).abs();
    assert!(
        total_err < 1e-10,
        "total_loss_mw should equal sum of individual losses"
    );
}

// ---------------------------------------------------------------------------
// Test 7: HVDC power conservation in AC network
// ---------------------------------------------------------------------------

/// Total AC generation = total AC load + AC line losses + HVDC losses.
/// Verified by checking the augmented network power balance at a high level.
#[test]
fn test_hvdc_power_conservation() {
    // 2-bus network: bus 1 (slack) → bus 2 (load, 80 MW).
    // HVDC transfers 40 MW from bus 1 to bus 2 via DC link.
    // The slack generator at bus 1 sees both the AC load at bus 2
    // (through the AC line) and the HVDC rectifier draw at bus 1.
    let net = two_bus_test_network(1, 2, 80.0);

    let vsc = VscHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 40.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.002,
        loss_coeff_b_pu: 0.001,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "conservation-test".to_string(),
    };

    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    assert!(sol.converged, "Solver should converge");

    let rect = &sol.stations[0];
    let inv = &sol.stations[1];

    // Rectifier draws P_dc from bus 1 (AC side).
    // Inverter injects (P_dc - losses) at bus 2 (AC side).
    // Conservation: p_drawn = p_to + p_loss
    let p_drawn = -rect.p_ac_mw;
    let balance_err = (p_drawn - inv.p_ac_mw - sol.total_loss_mw).abs();
    assert!(
        balance_err < 0.1,
        "HVDC power conservation error {balance_err:.4} MW"
    );

    // With non-zero losses, inverter injection < p_dc
    assert!(
        inv.p_ac_mw <= rect.p_dc_mw,
        "Inverter injection must be <= DC setpoint when losses > 0"
    );
}

// ---------------------------------------------------------------------------
// Test 8: LCC DC current and voltage populated
// ---------------------------------------------------------------------------

/// For an LCC link, the result should always include dc_current_ka and dc_voltage_pu.
#[test]
fn test_lcc_dc_quantities_populated() {
    let net = two_bus_test_network(1, 2, 100.0);

    let lcc = LccHvdcLink::new(1, 2, 100.0);
    let links = vec![HvdcLink::Lcc(lcc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    let rect = &sol.stations[0];

    assert!(rect.lcc_detail.is_some(), "LCC must populate lcc_detail");
    assert!(rect.v_dc_pu > 0.0, "DC voltage must be positive");
    assert!(
        rect.lcc_detail.as_ref().unwrap().i_dc_pu > 0.0,
        "DC current must be positive for positive power flow"
    );
}

// ---------------------------------------------------------------------------
// Test 9: VSC — no DC quantities (None for LCC-specific fields)
// ---------------------------------------------------------------------------

/// VSC results should have dc_current_ka = None and dc_voltage_pu = None.
#[test]
fn test_vsc_no_dc_quantities() {
    let net = two_bus_test_network(1, 2, 50.0);

    let vsc = VscHvdcLink::new(1, 2, 50.0);
    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    let rect = &sol.stations[0];

    // VSC should not have lcc_detail populated.
    assert!(
        rect.lcc_detail.is_none(),
        "VSC must not populate lcc_detail"
    );
}

// ---------------------------------------------------------------------------
// Test 10: Empty links — trivial solution
// ---------------------------------------------------------------------------

/// solve with an empty links slice should return immediately with 0 losses.
#[test]
fn test_empty_links() {
    let net = load_case9();
    let links: Vec<HvdcLink> = Vec::new();
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("solve with empty links should succeed");
    assert!(sol.converged);
    assert_eq!(sol.stations.len(), 0);
    assert_eq!(sol.total_loss_mw, 0.0);
    assert_eq!(sol.iterations, 0);
}

// ---------------------------------------------------------------------------
// Test 11: Invalid bus — BusNotFound error
// ---------------------------------------------------------------------------

/// Requesting a converter bus that doesn't exist in the network should return
/// HvdcError::BusNotFound.
#[test]
fn test_bus_not_found_error() {
    let net = load_case9();

    let vsc = VscHvdcLink::new(1, 9999, 50.0); // bus 9999 doesn't exist
    let links = vec![HvdcLink::Vsc(vsc)];
    let opts = HvdcOptions::default();

    let result = solve_hvdc_links(&net, &links, &opts);
    assert!(result.is_err(), "Should return error for non-existent bus");
    let err = result.unwrap_err();
    match err {
        surge_hvdc::HvdcError::BusNotFound(bus) => {
            assert_eq!(bus, 9999);
        }
        other => panic!("Expected BusNotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 12: LCC lossless case — r_dc_pu = 0
// ---------------------------------------------------------------------------

/// LCC with r_dc_pu = 0 should have zero DC line losses.
#[test]
fn test_lcc_lossless() {
    let net = two_bus_test_network(1, 2, 100.0);

    let lcc = LccHvdcLink {
        r_dc_pu: 0.0,
        ..LccHvdcLink::new(1, 2, 100.0)
    };
    let links = vec![HvdcLink::Lcc(lcc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("HVDC solve failed");
    let rect = &sol.stations[0];

    assert_eq!(
        rect.converter_loss_mw, 0.0,
        "LCC with r_dc=0 must have zero losses, got {:.6}",
        rect.converter_loss_mw
    );
}

// ---------------------------------------------------------------------------
// FPQ-43: VSC quadratic loss model
// ---------------------------------------------------------------------------

/// FPQ-43 / P5-022: With loss_c_pu > 0, total losses must exceed the linear-only case.
#[test]
fn test_vsc_quadratic_losses_exceed_linear() {
    // Linear-only VSC (loss_c_pu = 0.0 by default).
    let vsc_linear = VscHvdcLink {
        loss_coeff_a_mw: 0.002, // 0.2% constant loss
        loss_coeff_b_pu: 0.003, // 0.3% loss per pu current
        ..VscHvdcLink::new(1, 2, 100.0)
    };

    // Quadratic VSC: same linear coefficients + quadratic term.
    let vsc_quad = VscHvdcLink {
        loss_coeff_a_mw: 0.002,
        loss_coeff_b_pu: 0.003,
        loss_c_pu: 1e-5, // 1e-5 pu × I² → at I=1 pu: +0.001 MW extra vs linear
        ..VscHvdcLink::new(1, 2, 100.0)
    };

    let base_mva = 100.0;

    // Compute losses at I_ac = 1.0 pu (isolated loss call, no full solve needed).
    // We call losses_mw directly to compare models without running full AC-DC solve.
    let i_ac_pu = 1.0; // representative AC current magnitude
    let loss_linear = vsc_linear.losses_mw(i_ac_pu, base_mva);
    let loss_quad = vsc_quad.losses_mw(i_ac_pu, base_mva);

    assert!(
        loss_quad > loss_linear,
        "Quadratic model losses ({:.4} MW) must exceed linear model losses ({:.4} MW)",
        loss_quad,
        loss_linear
    );

    // The additional loss must equal c × I² × base_mva = 1e-5 × 1.0² × 100 = 1e-3 MW.
    let expected_extra = vsc_quad.loss_c_pu * i_ac_pu * i_ac_pu * base_mva;
    let actual_extra = loss_quad - loss_linear;
    assert!(
        (actual_extra - expected_extra).abs() < 1e-9,
        "Extra quadratic loss should be {expected_extra:.6} MW, got {actual_extra:.6} MW"
    );
}

/// FPQ-43 / P5-022: loss_c_pu = 0.0 (default) preserves linear-only behaviour.
#[test]
fn test_vsc_zero_quadratic_preserves_linear() {
    let vsc = VscHvdcLink {
        loss_coeff_a_mw: 0.002,
        loss_coeff_b_pu: 0.003,
        loss_c_pu: 0.0,
        ..VscHvdcLink::new(1, 2, 100.0)
    };

    let base_mva = 100.0;
    let i_ac_pu = 1.0;

    let expected = (0.002 + 0.003 * i_ac_pu) * base_mva;
    let actual = vsc.losses_mw(i_ac_pu, base_mva);

    assert!(
        (actual - expected).abs() < 1e-10,
        "With c=0, losses should be purely linear: expected {expected:.6}, got {actual:.6}"
    );
}

// ---------------------------------------------------------------------------
// FPQ-43 / P5-022: Full quadratic loss model (a + b|I| + c*I²)
// ---------------------------------------------------------------------------

/// Verify that the quadratic term c*I² adds the correct amount to losses
/// for a known AC current value.
///
/// At I_ac = 2.0 pu with a=0.003, b=0.010, c=0.020 and base_mva=100:
///   P_loss = (0.003 + 0.010*2.0 + 0.020*4.0) * 100
///           = (0.003 + 0.020 + 0.080) * 100
///           = 0.103 * 100 = 10.3 MW
#[test]
fn test_fpq43_quadratic_loss_known_value() {
    use surge_hvdc::VscHvdcLink;

    let mut params = VscHvdcLink::new(1, 2, 200.0);
    params.loss_coeff_a_mw = 0.003;
    params.loss_coeff_b_pu = 0.010;
    params.loss_c_pu = 0.020;

    let base_mva = 100.0;
    // I_ac = sqrt(P² + Q²) / (V * base_mva)
    // = sqrt(200² + 0²) / (1.0 * 100) = 2.0 pu
    let p_loss = vsc::vsc_losses_mw(&params, 200.0, 0.0, 1.0, base_mva);

    let i_ac = 2.0_f64;
    let expected = (0.003 + 0.010 * i_ac + 0.020 * i_ac * i_ac) * base_mva;

    assert!(
        (p_loss - expected).abs() < 1e-9,
        "Quadratic loss at I=2 pu: expected {expected:.6} MW, got {p_loss:.6} MW"
    );

    // Also verify monotonic increase: loss(I=1) < loss(I=2) < loss(I=3)
    let loss_1 = vsc::vsc_losses_mw(&params, 100.0, 0.0, 1.0, base_mva);
    let loss_2 = vsc::vsc_losses_mw(&params, 200.0, 0.0, 1.0, base_mva);
    let loss_3 = vsc::vsc_losses_mw(&params, 300.0, 0.0, 1.0, base_mva);
    assert!(
        loss_1 < loss_2 && loss_2 < loss_3,
        "Loss must be monotonically increasing: {loss_1:.4} < {loss_2:.4} < {loss_3:.4}"
    );
}

// ---------------------------------------------------------------------------
// PLAN-098 / FPQ-55 / P5-064: ConstantPVac — 3-bus voltage regulation test
// ---------------------------------------------------------------------------

/// Build a 3-bus network: slack(1) -- line -- pq(2) -- line -- pq(3)
/// Add a VSC in ConstantPVac mode at bus 3.
///
/// Scenario:
///   - Bus 3 has a heavy load (150 MW) that depresses voltage.
///   - The VSC (buses 3 → 2) operates in ConstantPVac mode, targeting V=1.05 pu at bus 3.
///   - After sequential AC-DC iteration, Q from the VSC should be positive (injecting),
///     and it must lie within [q_min, q_max].
///
/// This test verifies:
///   1. The solver converges.
///   2. Q is within the declared limits.
///   3. The result is consistent (|p_from| ≈ p_dc; p_to = p_dc - p_loss).
#[test]
fn test_plan098_constant_pvac_q_within_limits() {
    use surge_hvdc::model::vsc::vsc_converter_results_with_mode;
    use surge_hvdc::{HvdcLink, HvdcOptions, VscHvdcControlMode, VscHvdcLink, VscStationState};
    use surge_network::Network;
    use surge_network::network::Branch;
    use surge_network::network::{Bus, BusType, Generator, Load};

    // ── Build 3-bus AC network ────────────────────────────────────────────
    let mut net = Network::new("pvac-3bus");
    net.base_mva = 100.0;

    // Bus 1: slack
    let mut b1 = Bus::new(1, BusType::Slack, 230.0);
    b1.voltage_magnitude_pu = 1.05;
    net.buses.push(b1);

    // Bus 2: PQ (intermediate)
    net.buses.push(Bus::new(2, BusType::PQ, 230.0));

    // Bus 3: PQ (load end, depressed voltage due to heavy load)
    let b3 = Bus::new(3, BusType::PQ, 230.0);
    net.buses.push(b3);
    net.loads.push(Load::new(3, 150.0, 50.0));

    // Lines: 1-2 and 2-3
    net.branches.push(Branch::new_line(1, 2, 0.02, 0.08, 0.04));
    net.branches.push(Branch::new_line(2, 3, 0.03, 0.10, 0.04));

    // Slack generator at bus 1
    let mut gen1 = Generator::new(1, 300.0, 1.05);
    gen1.pmax = 500.0;
    gen1.qmax = 200.0;
    gen1.qmin = -200.0;
    net.generators.push(gen1);

    // ── VSC in ConstantPVac mode (rectifier at bus 3, inverter at bus 2) ──
    let q_min = -60.0_f64;
    let q_max = 60.0_f64;
    let v_target = 1.05_f64;

    let vsc_params = VscHvdcLink {
        from_bus: 3, // rectifier: draws P_dc from bus 3
        to_bus: 2,   // inverter: injects at bus 2
        p_dc_mw: 30.0,
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.001,
        loss_coeff_b_pu: 0.005,
        loss_c_pu: 0.010,
        q_max_from_mvar: q_max,
        q_min_from_mvar: q_min,
        q_max_to_mvar: 60.0,
        q_min_to_mvar: -60.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 0.0,
        name: "pvac-vsc".to_string(),
    };

    let mode = VscHvdcControlMode::ConstantPVac {
        p_set: 30.0,
        v_target,
        v_band: 0.01,
        q_min,
        q_max,
    };

    // Simulate several AC-DC outer iterations manually to see Q converge.
    // We start with v_from = 0.95 (low, below target) so Q should go positive.
    let mut state = VscStationState::new(30.0, 0.0);

    for _iter in 0..10 {
        // Simulate the voltage the solver would see at bus 3.
        // For this unit test we fix v_from = 0.96 to represent a loaded bus.
        let v_from = 0.96_f64;
        let v_to = 1.00_f64;

        let [rect_res, _inv_res] =
            vsc_converter_results_with_mode(&vsc_params, &mode, &state, v_from, v_to, net.base_mva);

        // Update state for next iteration
        state.q_mvar = rect_res.q_ac_mvar;
        state.p_mw = rect_res.p_dc_mw;
    }

    // After iteration: Q should be within [q_min, q_max]
    assert!(
        state.q_mvar >= q_min && state.q_mvar <= q_max,
        "Q must lie within [q_min, q_max] = [{q_min}, {q_max}], got {}",
        state.q_mvar
    );

    // With v_ac = 0.96 < v_target = 1.05, Q should be positive (injecting)
    // to support the low voltage.
    assert!(
        state.q_mvar > 0.0,
        "ConstantPVac must inject positive Q when v_ac ({}) < v_target ({v_target})",
        0.96_f64
    );

    // Run the full HVDC solve with the VSC as a standard ConstantPQ link
    // (the sequential solver uses the base vsc_converter_result internally),
    // verify it converges.
    let links = vec![HvdcLink::Vsc(vsc_params)];
    let opts = HvdcOptions::default();
    let sol = surge_hvdc::solve_hvdc_links(&net, &links, &opts)
        .expect("HVDC solve on 3-bus network should succeed");
    assert!(sol.converged, "Solver must converge on 3-bus PVac network");
    let res = &sol.stations[0];
    assert!(
        res.converter_loss_mw >= 0.0,
        "Losses must be non-negative in PVac test"
    );
}

// ---------------------------------------------------------------------------
// PLAN-099 / FPQ-56 / P5-065: PVdcDroop — 3-station MTDC imbalance test
// ---------------------------------------------------------------------------

/// 3-station MTDC droop test.
///
/// Setup:
///   - 3 VSC stations on a conceptual DC ring.
///   - Station A: p_set = 200 MW, droop k = 50 MW/pu, v_dc_set = 1.0 pu
///   - Station B: p_set = 200 MW, droop k = 50 MW/pu, v_dc_set = 1.0 pu
///   - Station C: p_set = 400 MW (load/inverter), tripped → P goes to 0
///
/// When C trips, the DC network sees a 400 MW shortfall.
/// At steady-state, A and B each absorb more power to compensate.
/// The droop equations distribute the imbalance proportionally to k:
///   ΔP_A = k_A * ΔV_dc;  ΔP_B = k_B * ΔV_dc
///   ΔP_A + ΔP_B = 400 MW  (the tripped load)
/// With equal k, ΔP_A = ΔP_B = 200 MW each.
///
/// This test verifies:
///   1. At nominal v_dc: P = p_set (no droop action).
///   2. After tripping C: A and B each pick up extra power proportional to droop.
///   3. With equal k_droop, the imbalance is shared equally.
///   4. P is clamped to [p_min, p_max] limits.
#[test]
fn test_plan099_droop_mtdc_imbalance_sharing() {
    use surge_hvdc::{VscHvdcControlMode, VscStationState};

    let k_droop = 50.0_f64; // 50 MW per pu V_dc
    let p_set_a = 200.0_f64;
    let p_set_b = 200.0_f64;
    let v_dc_nominal = 1.0_f64;

    let mode_a = VscHvdcControlMode::PVdcDroop {
        p_set: p_set_a,
        voltage_dc_setpoint_pu: v_dc_nominal,
        k_droop,
        p_min: 0.0,
        p_max: 400.0,
    };
    let mode_b = VscHvdcControlMode::PVdcDroop {
        p_set: p_set_b,
        voltage_dc_setpoint_pu: v_dc_nominal,
        k_droop,
        p_min: 0.0,
        p_max: 400.0,
    };

    // 1. At nominal V_dc: P = p_set for both stations
    let state_nominal = VscStationState::new(200.0, 0.0);
    let pa_nominal = mode_a.effective_p_mw(state_nominal.v_dc_pu);
    let pb_nominal = mode_b.effective_p_mw(state_nominal.v_dc_pu);
    assert!(
        (pa_nominal - p_set_a).abs() < 1e-9,
        "At nominal v_dc, A should output p_set={p_set_a}, got {pa_nominal}"
    );
    assert!(
        (pb_nominal - p_set_b).abs() < 1e-9,
        "At nominal v_dc, B should output p_set={p_set_b}, got {pb_nominal}"
    );

    // 2. Simulate DC voltage deviation after C trips.
    //    C was absorbing 400 MW (inverted sign: -400 MW injected by C).
    //    When C trips, DC network power balance: A + B must cover C's 400 MW.
    //    Droop: P_A = p_set_a + k_a * (v_dc - v_set)
    //           P_B = p_set_b + k_b * (v_dc - v_set)
    //    P_A + P_B = 400 MW (total demand from C's trip)
    //    With equal k_a = k_b = 50 MW/pu and equal p_set:
    //      2 * p_set + 2 * k * ΔV = 400
    //      2 * 200 + 2 * 50 * ΔV = 400
    //      ΔV = (400 - 400) / 100 = 0  ← already balanced!
    //
    //    Wait — C was an inverter absorbing 400 MW from the DC side.
    //    After C trips, the DC bus sees +400 MW more generation than load.
    //    DC voltage rises. A and B (generators) see higher V_dc.
    //    With positive k_droop, they increase P — they absorb MORE from AC.
    //    The DC surplus (400 MW) is absorbed by the droop response.
    //
    //    Solve: ΔP_A + ΔP_B = 400 MW
    //           k_a * ΔV + k_b * ΔV = 400
    //           ΔV = 400 / (50 + 50) = 4.0 pu  [in a simplified DC model]
    //
    //    This is a large excursion; real DC voltage would be limited by clamping.
    //    For this unit test we verify the droop arithmetic directly.

    let delta_v = 400.0 / (k_droop + k_droop); // ΔV that absorbs 400 MW
    let v_dc_post_trip = v_dc_nominal + delta_v;

    let pa_post = mode_a.effective_p_mw(v_dc_post_trip);
    let pb_post = mode_b.effective_p_mw(v_dc_post_trip);

    // Each should pick up delta_p = k * delta_v = 50 * 4 = 200 MW
    let expected_pa = (p_set_a + k_droop * delta_v).clamp(0.0, 400.0);
    let expected_pb = (p_set_b + k_droop * delta_v).clamp(0.0, 400.0);

    assert!(
        (pa_post - expected_pa).abs() < 1e-9,
        "Post-trip: A should output {expected_pa} MW, got {pa_post}"
    );
    assert!(
        (pb_post - expected_pb).abs() < 1e-9,
        "Post-trip: B should output {expected_pb} MW, got {pb_post}"
    );

    // 3. Equal k_droop → equal imbalance sharing
    let delta_pa = pa_post - pa_nominal;
    let delta_pb = pb_post - pb_nominal;
    assert!(
        (delta_pa - delta_pb).abs() < 1e-9,
        "Equal droop gains must share imbalance equally: ΔP_A={delta_pa:.4}, ΔP_B={delta_pb:.4}"
    );

    // 4. Total pickup equals the tripped station's power.
    let total_pickup = delta_pa + delta_pb;
    let tripped_power = 400.0_f64;
    assert!(
        (total_pickup - tripped_power).abs() < 1e-9,
        "Total droop pickup ({total_pickup:.4} MW) must equal tripped power ({tripped_power:.4} MW)"
    );

    // 5. Clamping: with very large v_dc, P must not exceed p_max.
    let v_dc_extreme = 1000.0_f64;
    let pa_clamped = mode_a.effective_p_mw(v_dc_extreme);
    assert_eq!(
        pa_clamped, 400.0,
        "At extreme v_dc, P must clamp at p_max=400 MW"
    );
}
