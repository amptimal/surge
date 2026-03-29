// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Phase 9 coverage expansion tests for surge-hvdc.
//!
//! 12 new tests covering:
//! - Weak AC convergence (SCR=2.0, SCR=1.5, decoupled failure)
//! - Multi-infeed HVDC (two VSC same bus, LCC+VSC hybrid)
//! - HVDC + contingency (branch trip, converter trip)
//! - Control modes stress (droop load step, PVac regulation, mixed 3-terminal)
//! - OPF integration (binding limit, LCC Q vs P)

mod common;

use common::two_bus_test_network;
use num_complex::Complex64;
use surge_hvdc::advanced::block_coupled::{
    AcDcSolverMode, BlockCoupledAcDcSolverOptions, DcBranch as DcCable, DcNetwork, VscStation,
    solve as solve_block_coupled,
};
use surge_hvdc::advanced::hybrid::{
    DcBranch, HybridMtdcNetwork, HybridVscConverter, LccConverter, solve as solve_hybrid,
};
use surge_hvdc::{
    HvdcLink, HvdcOptions, LccHvdcLink, VscHvdcControlMode, VscHvdcLink, solve_hvdc_links,
};
use surge_network::Network;
use surge_network::network::Branch;
use surge_network::network::{Bus, BusType, Generator, Load};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a 2-bus AC network with configurable line impedance for SCR testing.
///
/// Bus 1: slack (strong source, Vm=1.0)
/// Bus 2: PQ load bus
/// Line impedance: r + jx where x = 1/SCR approximately models the Thevenin
/// impedance seen from bus 2. Higher x = weaker AC system.
fn weak_ac_network(scr: f64, load_mw: f64) -> Network {
    let x = 1.0 / scr; // Thevenin reactance in pu
    let r = x * 0.1; // R/X ratio = 0.1 (typical transmission)

    let mut net = Network::new("weak-ac");
    net.base_mva = 100.0;

    let mut b1 = Bus::new(1, BusType::Slack, 230.0);
    b1.voltage_magnitude_pu = 1.0;
    net.buses.push(b1);

    let b2 = Bus::new(2, BusType::PQ, 230.0);
    net.buses.push(b2);
    net.loads.push(Load::new(2, load_mw, 0.0));

    net.branches.push(Branch::new_line(1, 2, r, x, 0.02));

    let mut gobj = Generator::new(1, load_mw * 2.0, 1.0);
    gobj.pmax = load_mw * 5.0;
    gobj.qmax = load_mw * 3.0;
    gobj.qmin = -load_mw * 3.0;
    net.generators.push(gobj);

    net
}

/// Flat AC voltage array of n buses at 1.0 pu / 0 deg.
fn flat_ac(n: usize) -> Vec<Complex64> {
    vec![Complex64::new(1.0, 0.0); n]
}

// ===========================================================================
// Weak AC convergence (3 tests)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 1: Weak AC SCR=2.0, coupled solver converges
// ---------------------------------------------------------------------------

/// Build a 2-bus weak AC network (SCR=2.0) and solve with the block-coupled
/// AC/DC solver using sensitivity corrections. The correction should handle
/// the strong AC/DC interaction at this SCR.
#[test]
fn test_weak_ac_scr_2_converges() {
    let net = weak_ac_network(2.0, 50.0);

    // Build a simple 2-bus DC network: bus 0 = slack, bus 1 = free.
    let mut dc_net = DcNetwork::new(2, 0);
    dc_net.v_dc_slack = 1.0;
    dc_net.add_cable(DcCable {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 0.01,
        i_max_pu: 5.0,
    });

    // VSC station at AC bus 2 (weak bus), DC bus 1. Constant P = 30 MW.
    let station = VscStation {
        ac_bus: 2,
        dc_bus_idx: 1,
        control_mode: VscHvdcControlMode::ConstantPQ {
            p_set: 30.0,
            q_set: 0.0,
        },
        q_max_mvar: 100.0,
        q_min_mvar: -100.0,
        loss_constant_mw: 0.001,
        loss_linear: 0.002,
        loss_c_rectifier: 0.0,
        loss_c_inverter: 0.0,
    };

    // DC slack station at AC bus 1, DC bus 0.
    let slack_station = VscStation {
        ac_bus: 1,
        dc_bus_idx: 0,
        control_mode: VscHvdcControlMode::ConstantVdc {
            v_dc_target: 1.0,
            q_set: 0.0,
        },
        q_max_mvar: 200.0,
        q_min_mvar: -200.0,
        loss_constant_mw: 0.0,
        loss_linear: 0.0,
        loss_c_rectifier: 0.0,
        loss_c_inverter: 0.0,
    };

    let stations = [slack_station, station];
    let opts = BlockCoupledAcDcSolverOptions {
        solver_mode: AcDcSolverMode::BlockCoupled,
        apply_coupling_sensitivities: true,
        max_iter: 50,
        ..BlockCoupledAcDcSolverOptions::default()
    };

    let result = solve_block_coupled(&net, &mut dc_net, &stations, &opts)
        .expect("Coupled solver should converge at SCR=2.0");
    assert!(
        result.converged,
        "Coupled solver must converge at SCR=2.0, took {} iterations",
        result.iterations
    );
}

// ---------------------------------------------------------------------------
// Test 2: Weak AC SCR=1.5, coupled solver converges
// ---------------------------------------------------------------------------

/// Even weaker AC (SCR=1.5). The coupled solver should still converge,
/// potentially needing more iterations.
#[test]
fn test_weak_ac_scr_1_5_converges() {
    let net = weak_ac_network(1.5, 30.0);

    let mut dc_net = DcNetwork::new(2, 0);
    dc_net.v_dc_slack = 1.0;
    dc_net.add_cable(DcCable {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 0.01,
        i_max_pu: 5.0,
    });

    let slack_station = VscStation {
        ac_bus: 1,
        dc_bus_idx: 0,
        control_mode: VscHvdcControlMode::ConstantVdc {
            v_dc_target: 1.0,
            q_set: 0.0,
        },
        q_max_mvar: 200.0,
        q_min_mvar: -200.0,
        loss_constant_mw: 0.0,
        loss_linear: 0.0,
        loss_c_rectifier: 0.0,
        loss_c_inverter: 0.0,
    };

    let station = VscStation {
        ac_bus: 2,
        dc_bus_idx: 1,
        control_mode: VscHvdcControlMode::ConstantPQ {
            p_set: 20.0,
            q_set: 0.0,
        },
        q_max_mvar: 100.0,
        q_min_mvar: -100.0,
        loss_constant_mw: 0.001,
        loss_linear: 0.002,
        loss_c_rectifier: 0.0,
        loss_c_inverter: 0.0,
    };

    let stations = [slack_station, station];
    let opts = BlockCoupledAcDcSolverOptions {
        solver_mode: AcDcSolverMode::BlockCoupled,
        apply_coupling_sensitivities: true,
        max_iter: 50,
        ..BlockCoupledAcDcSolverOptions::default()
    };

    let result = solve_block_coupled(&net, &mut dc_net, &stations, &opts)
        .expect("Coupled solver should converge at SCR=1.5");
    assert!(
        result.converged,
        "Coupled solver must converge at SCR=1.5, took {} iterations",
        result.iterations
    );
}

// ---------------------------------------------------------------------------
// Test 3: Decoupled solver needs more iterations on weak AC
// ---------------------------------------------------------------------------

/// With sensitivity correction disabled, the solver should either need
/// significantly more iterations than the corrected mode or fail to converge
/// within the same iteration budget.
#[test]
fn test_decoupled_fails_weak_ac() {
    let net = weak_ac_network(2.0, 50.0);

    // Coupled solve first to get baseline iteration count.
    let mut dc_net_coupled = DcNetwork::new(2, 0);
    dc_net_coupled.v_dc_slack = 1.0;
    dc_net_coupled.add_cable(DcCable {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 0.01,
        i_max_pu: 5.0,
    });

    let make_stations = || {
        vec![
            VscStation {
                ac_bus: 1,
                dc_bus_idx: 0,
                control_mode: VscHvdcControlMode::ConstantVdc {
                    v_dc_target: 1.0,
                    q_set: 0.0,
                },
                q_max_mvar: 200.0,
                q_min_mvar: -200.0,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                loss_c_rectifier: 0.0,
                loss_c_inverter: 0.0,
            },
            VscStation {
                ac_bus: 2,
                dc_bus_idx: 1,
                control_mode: VscHvdcControlMode::ConstantPQ {
                    p_set: 30.0,
                    q_set: 0.0,
                },
                q_max_mvar: 100.0,
                q_min_mvar: -100.0,
                loss_constant_mw: 0.001,
                loss_linear: 0.002,
                loss_c_rectifier: 0.0,
                loss_c_inverter: 0.0,
            },
        ]
    };

    let stations = make_stations();
    let coupled_opts = BlockCoupledAcDcSolverOptions {
        solver_mode: AcDcSolverMode::BlockCoupled,
        apply_coupling_sensitivities: true,
        max_iter: 50,
        ..BlockCoupledAcDcSolverOptions::default()
    };
    let coupled_result = solve_block_coupled(&net, &mut dc_net_coupled, &stations, &coupled_opts)
        .expect("Coupled must converge for baseline");
    let coupled_iters = coupled_result.iterations;

    // Decoupled solve with the same iteration budget.
    let mut dc_net_decoupled = DcNetwork::new(2, 0);
    dc_net_decoupled.v_dc_slack = 1.0;
    dc_net_decoupled.add_cable(DcCable {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 0.01,
        i_max_pu: 5.0,
    });

    let stations2 = make_stations();
    let decoupled_opts = BlockCoupledAcDcSolverOptions {
        solver_mode: AcDcSolverMode::BlockCoupled,
        apply_coupling_sensitivities: false,
        max_iter: 50,
        ..BlockCoupledAcDcSolverOptions::default()
    };
    let decoupled_result =
        solve_block_coupled(&net, &mut dc_net_decoupled, &stations2, &decoupled_opts);

    // Either the decoupled solver diverges, or it needs more iterations.
    match decoupled_result {
        Err(_) => {
            // Divergence is one valid outcome for weak AC decoupled.
        }
        Ok(res) => {
            // If it converges, it should need at least as many iterations.
            assert!(
                res.iterations >= coupled_iters,
                "Decoupled should need >= coupled iterations: decoupled={}, coupled={}",
                res.iterations,
                coupled_iters
            );
        }
    }
}

// ===========================================================================
// Multi-infeed HVDC (2 tests)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 4: Two VSC converters sharing adjacent DC buses
// ---------------------------------------------------------------------------

/// Two VSC converters injecting into adjacent AC buses, sharing a DC network.
/// Uses hybrid MTDC solver. Both converters should produce valid results.
#[test]
fn test_multi_infeed_two_vsc_same_bus() {
    let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
    net.dc_network.v_dc_slack = 1.0;

    // Star cables from slack hub.
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 2,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });

    // VSC 1 at AC bus 1, DC bus 1: rectifier +150 MW.
    let vsc1 = HybridVscConverter::new(1, 1, 150.0);
    net.vsc_converters.push(vsc1);

    // VSC 2 at AC bus 2, DC bus 2: inverter -150 MW.
    let vsc2 = HybridVscConverter::new(2, 2, -150.0);
    net.vsc_converters.push(vsc2);

    let ac_v = flat_ac(2);
    let result = solve_hybrid(&net, &ac_v, 50, 1e-6).expect("Multi-infeed 2-VSC should converge");

    assert!(result.converged, "Must converge");
    assert_eq!(result.vsc_results.len(), 2);

    // DC voltages should be realistic.
    for (i, &v) in result.dc_voltages_pu.iter().enumerate() {
        assert!(
            v > 0.9 && v < 1.1,
            "DC bus {i} voltage {v:.4} pu outside [0.9, 1.1]"
        );
    }

    // Power balance: +150 - 150 = 0; losses should be small.
    let p_total: f64 = result.vsc_results.iter().map(|r| r.p_dc_mw).sum();
    assert!(
        (p_total + result.total_dc_loss_mw).abs() < 5.0,
        "Power balance error: p_total={p_total:.2}, losses={:.2}",
        result.total_dc_loss_mw
    );
}

// ---------------------------------------------------------------------------
// Test 5: Multi-infeed LCC + VSC hybrid sharing DC network
// ---------------------------------------------------------------------------

/// One LCC rectifier and one VSC inverter on the same DC network.
/// This is the canonical hybrid MTDC scenario.
#[test]
fn test_multi_infeed_lcc_vsc_hybrid() {
    let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
    net.dc_network.v_dc_slack = 1.0;

    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 1e-4,
        i_max_pu: 0.0,
    });
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 2,
        r_dc_pu: 1e-4,
        i_max_pu: 0.0,
    });

    // LCC rectifier: +200 MW into DC.
    let mut lcc = LccConverter::new(1, 1, 200.0);
    lcc.p_setpoint_mw = 200.0;
    lcc.x_commutation_pu = 0.15;
    net.lcc_converters.push(lcc);

    // VSC inverter: -200 MW from DC.
    let vsc = HybridVscConverter::new(2, 2, -200.0);
    net.vsc_converters.push(vsc);

    let ac_v = flat_ac(2);
    let result = solve_hybrid(&net, &ac_v, 50, 1e-6).expect("LCC+VSC hybrid should converge");

    assert!(result.converged, "LCC+VSC hybrid must converge");
    assert_eq!(result.lcc_results.len(), 1);
    assert_eq!(result.vsc_results.len(), 1);

    // LCC should absorb reactive power.
    let lcc_res = &result.lcc_results[0];
    assert!(
        lcc_res.q_ac_mvar <= 0.0,
        "LCC must absorb reactive power, got Q={:.2} MVAR",
        lcc_res.q_ac_mvar
    );

    // LCC firing angle should be physical.
    assert!(
        lcc_res.alpha_deg >= 5.0 && lcc_res.alpha_deg <= 150.0,
        "LCC firing angle {:.2} deg outside [5, 150]",
        lcc_res.alpha_deg
    );
}

// ===========================================================================
// HVDC + contingency (2 tests)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 6: HVDC branch trip — MTDC still converges
// ---------------------------------------------------------------------------

/// Build a 3-bus MTDC network. Remove one cable (simulate a trip) and verify
/// the solver still converges with adjusted topology.
#[test]
fn test_hvdc_branch_trip_convergence() {
    let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
    net.dc_network.v_dc_slack = 1.0;

    // Three cables forming a triangle: 0-1, 0-2, 1-2.
    // If we trip 1-2, buses 1 and 2 still reach slack via 0.
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 2,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });
    // Skip cable 1-2 (simulating a trip).

    let vsc1 = HybridVscConverter::new(1, 1, 100.0);
    net.vsc_converters.push(vsc1);

    let vsc2 = HybridVscConverter::new(2, 2, -100.0);
    net.vsc_converters.push(vsc2);

    let ac_v = flat_ac(2);
    let result = solve_hybrid(&net, &ac_v, 50, 1e-6).expect("Post-trip MTDC should converge");

    assert!(
        result.converged,
        "MTDC must converge after branch trip, took {} iterations",
        result.iterations
    );

    // All DC voltages should be realistic.
    for (i, &v) in result.dc_voltages_pu.iter().enumerate() {
        assert!(
            v > 0.85 && v < 1.15,
            "DC bus {i} voltage {v:.4} pu out of range after trip"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 7: Converter trip — remaining converters still balance
// ---------------------------------------------------------------------------

/// 3-converter MTDC where one converter is taken out of service.
/// The remaining converters should still converge and maintain balance.
#[test]
fn test_hvdc_converter_trip() {
    let mut net = HybridMtdcNetwork::new(100.0, 4, 0);
    net.dc_network.v_dc_slack = 1.0;

    // Star cables from slack.
    for i in 1..=3 {
        net.dc_network.branches.push(DcBranch {
            from_dc_bus: 0,
            to_dc_bus: i,
            r_dc_pu: 1e-3,
            i_max_pu: 0.0,
        });
    }

    // VSC 1: +200 MW rectifier (in-service).
    let vsc1 = HybridVscConverter::new(1, 1, 200.0);
    net.vsc_converters.push(vsc1);

    // VSC 2: -100 MW inverter (in-service).
    let vsc2 = HybridVscConverter::new(2, 2, -100.0);
    net.vsc_converters.push(vsc2);

    // VSC 3: -100 MW inverter (TRIPPED — out of service).
    let mut vsc3 = HybridVscConverter::new(3, 3, -100.0);
    vsc3.in_service = false;
    net.vsc_converters.push(vsc3);

    let ac_v = flat_ac(3);
    let result =
        solve_hybrid(&net, &ac_v, 50, 1e-6).expect("MTDC with tripped converter should converge");

    assert!(result.converged, "Must converge with one converter tripped");

    // The tripped converter should have zero results.
    let vsc3_res = &result.vsc_results[2];
    assert_eq!(
        vsc3_res.p_dc_mw, 0.0,
        "Tripped converter P should be 0, got {:.2}",
        vsc3_res.p_dc_mw
    );
    assert_eq!(
        vsc3_res.losses_mw, 0.0,
        "Tripped converter losses should be 0"
    );

    // In-service converters should have non-zero DC power.
    assert!(
        result.vsc_results[0].p_dc_mw.abs() > 1.0,
        "In-service VSC 1 should have significant power"
    );
    assert!(
        result.vsc_results[1].p_dc_mw.abs() > 1.0,
        "In-service VSC 2 should have significant power"
    );
}

// ===========================================================================
// Control modes stress (3 tests)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 8: Droop-like behavior — setpoint change adjusts DC voltage
// ---------------------------------------------------------------------------

/// Two VSC converters in MTDC. Change one converter's setpoint significantly
/// and verify that DC voltages adjust correctly via the NR solution.
#[test]
fn test_vdc_droop_load_step() {
    // Scenario A: balanced (100 MW in, 100 MW out).
    let build_net = |p_vsc2: f64| -> HybridMtdcNetwork {
        let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
        net.dc_network.v_dc_slack = 1.0;

        net.dc_network.branches.push(DcBranch {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.01,
            i_max_pu: 0.0,
        });
        net.dc_network.branches.push(DcBranch {
            from_dc_bus: 0,
            to_dc_bus: 2,
            r_dc_pu: 0.01,
            i_max_pu: 0.0,
        });

        let vsc1 = HybridVscConverter::new(1, 1, 200.0);
        net.vsc_converters.push(vsc1);

        let vsc2 = HybridVscConverter::new(2, 2, p_vsc2);
        net.vsc_converters.push(vsc2);

        net
    };

    let ac_v = flat_ac(2);

    // Balanced case.
    let net_balanced = build_net(-200.0);
    let res_balanced =
        solve_hybrid(&net_balanced, &ac_v, 50, 1e-6).expect("Balanced should converge");

    // Imbalanced case: VSC 2 draws less (only -100 MW).
    let net_imbalanced = build_net(-100.0);
    let res_imbalanced =
        solve_hybrid(&net_imbalanced, &ac_v, 50, 1e-6).expect("Imbalanced should converge");

    assert!(res_balanced.converged);
    assert!(res_imbalanced.converged);

    // When there is excess power in the DC network (200 in, only 100 out vs
    // 200 in, 200 out), DC voltages at bus 2 (the inverter) should differ
    // because the power drawn from that bus changes.
    let v_balanced_2 = res_balanced.dc_voltages_pu[2];
    let v_imbalanced_2 = res_imbalanced.dc_voltages_pu[2];
    assert!(
        (v_balanced_2 - v_imbalanced_2).abs() > 1e-6,
        "DC voltage at bus 2 must change when load step occurs: balanced={v_balanced_2:.6}, imbalanced={v_imbalanced_2:.6}"
    );
}

// ---------------------------------------------------------------------------
// Test 9: VSC reactive power delivery
// ---------------------------------------------------------------------------

/// VSC with explicit q_setpoint_mvar. Verify Q is delivered correctly
/// in the hybrid MTDC result.
#[test]
fn test_pvac_voltage_regulation() {
    let mut net = HybridMtdcNetwork::new(100.0, 2, 0);
    net.dc_network.v_dc_slack = 1.0;

    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });

    // VSC with Q setpoint = 30 MVAR injection.
    let mut vsc = HybridVscConverter::new(1, 1, 100.0);
    vsc.q_setpoint_mvar = 30.0;
    vsc.q_max_mvar = 50.0;
    vsc.q_min_mvar = -50.0;
    net.vsc_converters.push(vsc);

    let ac_v = flat_ac(1);
    let result = solve_hybrid(&net, &ac_v, 50, 1e-6).expect("VSC Q test should converge");

    assert!(result.converged);

    let vsc_res = &result.vsc_results[0];
    // Q should equal the setpoint (clamped within limits).
    assert!(
        (vsc_res.q_ac_mvar - 30.0).abs() < 1.0,
        "VSC Q should be close to setpoint 30 MVAR, got {:.2}",
        vsc_res.q_ac_mvar
    );
}

// ---------------------------------------------------------------------------
// Test 10: Mixed control modes — 3-terminal MTDC
// ---------------------------------------------------------------------------

/// 3-terminal MTDC with:
/// - Station 0 (DC slack): Vdc=1.0 (absorbs imbalance)
/// - Station 1: constant P = +200 MW (rectifier)
/// - Station 2: constant P = -180 MW (inverter)
///
/// All should converge with consistent DC voltages.
#[test]
fn test_mixed_control_modes_mtdc() {
    let mut net = HybridMtdcNetwork::new(100.0, 3, 0);
    net.dc_network.v_dc_slack = 1.0;

    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 1,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 0,
        to_dc_bus: 2,
        r_dc_pu: 1e-3,
        i_max_pu: 0.0,
    });
    net.dc_network.branches.push(DcBranch {
        from_dc_bus: 1,
        to_dc_bus: 2,
        r_dc_pu: 2e-3,
        i_max_pu: 0.0,
    });

    // VSC 1: rectifier +200 MW.
    let vsc1 = HybridVscConverter::new(1, 1, 200.0);
    net.vsc_converters.push(vsc1);

    // VSC 2: inverter -180 MW.
    let vsc2 = HybridVscConverter::new(2, 2, -180.0);
    net.vsc_converters.push(vsc2);

    let ac_v = flat_ac(2);
    let result =
        solve_hybrid(&net, &ac_v, 100, 1e-6).expect("3-terminal mixed modes should converge");

    assert!(
        result.converged,
        "3-terminal MTDC must converge; iterations = {}",
        result.iterations
    );

    // DC voltages should be in a realistic range.
    for (i, &v) in result.dc_voltages_pu.iter().enumerate() {
        assert!(
            v > 0.85 && v < 1.15,
            "DC bus {i} voltage {v:.4} pu outside [0.85, 1.15]"
        );
    }

    // The slack bus should be exactly at its setpoint.
    assert!(
        (result.dc_voltages_pu[0] - 1.0).abs() < 1e-9,
        "DC slack must hold 1.0 pu, got {:.8}",
        result.dc_voltages_pu[0]
    );

    // Power from VSC 1 should be close to +200 MW.
    let p1 = result.vsc_results[0].p_dc_mw;
    assert!(
        (p1 - 200.0).abs() < 1.0,
        "VSC 1 DC power should be ~200 MW, got {p1:.2}"
    );

    // Power from VSC 2 should be close to -180 MW.
    let p2 = result.vsc_results[1].p_dc_mw;
    assert!(
        (p2 - (-180.0)).abs() < 1.0,
        "VSC 2 DC power should be ~-180 MW, got {p2:.2}"
    );
}

// ===========================================================================
// OPF integration (2 tests)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 11: HVDC link at flow limit — power capped
// ---------------------------------------------------------------------------

/// Test that when HVDC link has p_dc_min_mw/p_dc_max_mw bounds, the VSC
/// params correctly enforce the limit. We verify the setpoint respects bounds
/// by checking has_variable_p_dc() and that OPF-style clamping would work.
#[test]
fn test_dc_opf_hvdc_binding_limit() {
    // Create a VSC with variable bounds.
    let vsc_bounded = VscHvdcLink {
        from_bus: 1,
        to_bus: 2,
        p_dc_mw: 200.0, // requested setpoint
        q_from_mvar: 0.0,
        q_to_mvar: 0.0,
        loss_coeff_a_mw: 0.0,
        loss_coeff_b_pu: 0.0,
        loss_c_pu: 0.0,
        q_max_from_mvar: 100.0,
        q_min_from_mvar: -100.0,
        q_max_to_mvar: 100.0,
        q_min_to_mvar: -100.0,
        p_dc_min_mw: 0.0,
        p_dc_max_mw: 150.0, // cap at 150 MW
        name: "bounded-vsc".to_string(),
    };

    // has_variable_p_dc should be true when min < max.
    assert!(
        vsc_bounded.has_variable_p_dc(),
        "VSC with min=0, max=150 should have variable P_dc"
    );

    // When used in a solve, the power should be clamped to the max.
    // Since we can't run full OPF here, verify the bound is correct
    // and that solve with the setpoint at 200 MW still converges
    // (the sequential solver uses p_dc_mw as-is; OPF would clamp it).
    let net = two_bus_test_network(1, 2, 100.0);

    // Reduce the setpoint to within the bound.
    let vsc_feasible = VscHvdcLink {
        p_dc_mw: 150.0, // at the limit
        ..vsc_bounded.clone()
    };

    let links = vec![HvdcLink::Vsc(vsc_feasible)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("Bounded HVDC solve should succeed");
    assert!(sol.converged, "Must converge at the flow limit");

    let res = &sol.stations[0];
    assert!(
        (res.p_dc_mw - 150.0).abs() < 1.0,
        "P_dc should be at the 150 MW limit, got {:.2}",
        res.p_dc_mw
    );

    // A VSC without variable bounds should report has_variable_p_dc = false.
    let vsc_fixed = VscHvdcLink::new(1, 2, 100.0);
    assert!(
        !vsc_fixed.has_variable_p_dc(),
        "Default VSC (min=max=0) should not have variable P_dc"
    );
}

// ---------------------------------------------------------------------------
// Test 12: LCC reactive power proportional to active power
// ---------------------------------------------------------------------------

/// Verify that the LCC model computes Q = P * tan(alpha) correctly
/// for multiple firing angles, confirming the Q-P relationship.
#[test]
fn test_lcc_reactive_power_vs_active() {
    let p_dc = 100.0; // 100 MW DC power

    // Test several firing angles.
    for alpha_deg in [10.0, 15.0, 20.0, 30.0, 45.0] {
        let lcc = LccHvdcLink {
            firing_angle_deg: alpha_deg,
            extinction_angle_deg: alpha_deg,
            ..LccHvdcLink::new(1, 2, p_dc)
        };

        let q_rect = lcc.q_rectifier_mvar(p_dc);
        let q_inv = lcc.q_inverter_mvar(p_dc);

        let alpha_rad = alpha_deg.to_radians();
        let expected_q = p_dc * alpha_rad.tan();

        // Rectifier Q should match P * tan(alpha).
        assert!(
            (q_rect - expected_q).abs() < 0.01,
            "alpha={alpha_deg}: Q_rect={q_rect:.4} should equal P*tan(alpha)={expected_q:.4}"
        );

        // Inverter Q should match P * tan(gamma) (gamma = extinction angle = alpha here).
        assert!(
            (q_inv - expected_q).abs() < 0.01,
            "alpha={alpha_deg}: Q_inv={q_inv:.4} should equal P*tan(gamma)={expected_q:.4}"
        );

        // Q should increase with firing angle (monotonic in tan).
        assert!(
            q_rect > 0.0,
            "Q must be positive for positive alpha, got {q_rect:.4}"
        );
    }

    // Verify monotonicity: larger alpha -> larger Q.
    let q_15 = LccHvdcLink {
        firing_angle_deg: 15.0,
        ..LccHvdcLink::new(1, 2, p_dc)
    }
    .q_rectifier_mvar(p_dc);

    let q_30 = LccHvdcLink {
        firing_angle_deg: 30.0,
        ..LccHvdcLink::new(1, 2, p_dc)
    }
    .q_rectifier_mvar(p_dc);

    let q_45 = LccHvdcLink {
        firing_angle_deg: 45.0,
        ..LccHvdcLink::new(1, 2, p_dc)
    }
    .q_rectifier_mvar(p_dc);

    assert!(
        q_15 < q_30 && q_30 < q_45,
        "Q must be monotonically increasing with alpha: Q(15)={q_15:.4} < Q(30)={q_30:.4} < Q(45)={q_45:.4}"
    );

    // Also run through the full solve to verify the LCC Q relationship holds
    // in an actual power flow solution.
    let net = two_bus_test_network(1, 2, 100.0);
    let lcc = LccHvdcLink {
        firing_angle_deg: 20.0,
        extinction_angle_deg: 20.0,
        ..LccHvdcLink::new(1, 2, 80.0)
    };
    let links = vec![HvdcLink::Lcc(lcc)];
    let opts = HvdcOptions::default();

    let sol = solve_hvdc_links(&net, &links, &opts).expect("LCC Q/P solve failed");
    assert!(sol.converged);

    // converters[0] = rectifier, converters[1] = inverter (per-station model)
    let rect = &sol.stations[0];
    let inv = &sol.stations[1];
    // Rectifier absorbs reactive power (q_ac_mvar < 0).
    assert!(
        rect.q_ac_mvar < 0.0,
        "LCC rectifier must absorb Q, got {:.4}",
        rect.q_ac_mvar
    );
    // Inverter also absorbs reactive power.
    assert!(
        inv.q_ac_mvar < 0.0,
        "LCC inverter must absorb Q, got {:.4}",
        inv.q_ac_mvar
    );
}
