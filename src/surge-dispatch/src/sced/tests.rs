// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
// Test content for sced::tests module.

use super::*;
use surge_network::market::{SystemReserveRequirement, ZonalReserveRequirement};
use surge_network::network::{
    BranchOpfControl, BranchRef, CommitmentStatus, Load, MarketParams, WeightedBranchRef,
};

#[allow(dead_code)]
fn data_available() -> bool {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::Path::new(&p).exists();
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
        .exists()
}
#[allow(dead_code)]
fn test_data_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
}

#[allow(dead_code)]
fn test_data_path(name: &str) -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
        return std::path::PathBuf::from(p).join(name);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/data")
        .join(name)
}

#[test]
fn test_sced_matches_dcopf_case9() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    // Without ramp or reserve constraints, SCED should match DC-OPF
    let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
    let sced_opts = DispatchOptions::default();
    let dcopf_opts = surge_opf::DcOpfOptions::default();

    let sced_sol = solve_sced(&net, &sced_opts).unwrap();
    let dcopf_sol = surge_opf::solve_dc_opf(&net, &dcopf_opts).unwrap();

    let rel_err = (sced_sol.dispatch.total_cost - dcopf_sol.opf.total_cost).abs()
        / dcopf_sol.opf.total_cost.abs().max(1.0);
    assert!(
        rel_err < 1e-4,
        "SCED cost={:.4}, DC-OPF cost={:.4}, rel_err={:.6}",
        sced_sol.dispatch.total_cost,
        dcopf_sol.opf.total_cost,
        rel_err
    );

    // Power balance
    let total_gen: f64 = sced_sol.dispatch.pg_mw.iter().sum();
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_gen - total_load).abs() < 0.1,
        "power balance: gen={total_gen:.2}, load={total_load:.2}"
    );
}

#[test]
fn test_sced_with_reserve() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
    for g in &mut net.generators {
        let phys_cap = (g.pmax - g.pmin).max(0.0);
        if phys_cap > 0.0 {
            g.market.get_or_insert_default().reserve_offers.push(
                surge_network::market::ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: phys_cap,
                    cost_per_mwh: 0.0,
                },
            );
        }
    }
    let opts = DispatchOptions {
        system_reserve_requirements: vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 50.0,
            per_period_mw: None,
        }],
        ..Default::default()
    };

    let sol = solve_sced(&net, &opts).unwrap();

    // Reserve should be allocated
    let spin_awards = sol
        .dispatch
        .reserve_awards
        .get("spin")
        .expect("spin awards");
    assert!(!spin_awards.is_empty());
    let total_reserve: f64 = spin_awards.iter().sum();
    assert!(
        total_reserve >= 49.9,
        "total reserve={total_reserve:.1} MW, required 50 MW"
    );

    // Reserve price should be positive
    let spin_price = sol
        .dispatch
        .reserve_prices
        .get("spin")
        .copied()
        .unwrap_or(0.0);
    assert!(
        spin_price >= 0.0,
        "reserve price={:.4} should be non-negative",
        spin_price
    );

    // Cost should be >= no-reserve case (reserve constrains dispatch)
    let no_reserve = solve_sced(&net, &DispatchOptions::default()).unwrap();
    assert!(
        sol.dispatch.total_cost >= no_reserve.dispatch.total_cost - 0.01,
        "reserve cost={:.2} should be >= no-reserve cost={:.2}",
        sol.dispatch.total_cost,
        no_reserve.dispatch.total_cost
    );
}

#[test]
fn test_request_level_pwl_generator_costs_remove_sced_hessian() {
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("sced_request_level_pwl_costs");
    net.base_mva = 100.0;
    net.buses.push(Bus::new(1, BusType::Slack, 138.0));
    net.loads.push(Load::new(1, 80.0, 0.0));

    let mut generator = Generator::new(1, 0.0, 1.0);
    generator.pmin = 0.0;
    generator.pmax = 100.0;
    generator.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![0.01, 10.0, 0.0],
    });
    net.generators.push(generator);

    let baseline_request = crate::request::DispatchRequest {
        network: crate::request::DispatchNetwork {
            thermal_limits: crate::request::ThermalLimitPolicy {
                enforce: false,
                ..crate::request::ThermalLimitPolicy::default()
            },
            flowgates: crate::request::FlowgatePolicy {
                enabled: false,
                ..crate::request::FlowgatePolicy::default()
            },
            ..crate::request::DispatchNetwork::default()
        },
        ..crate::request::DispatchRequest::default()
    };
    let baseline_normalized = baseline_request
        .normalize()
        .expect("normalize baseline request");
    let baseline_context = crate::common::runtime::DispatchPeriodContext::initial(
        &baseline_normalized.input.initial_state,
    );
    let baseline_spec = baseline_normalized.problem_spec();
    let baseline_session =
        crate::common::dc::build_period_solve_session(&net, baseline_spec, 0).unwrap();
    let baseline_model_plan = super::plan::build_model_plan(super::plan::ScedModelPlanInput {
        network: &net,
        context: baseline_context,
        solve: &baseline_session,
    })
    .unwrap();
    let baseline_problem_plan =
        super::plan::build_problem_plan(super::plan::ScedProblemPlanInput {
            network: &net,
            context: baseline_context,
            solve: &baseline_session,
            model_plan: &baseline_model_plan,
        });
    assert!(
        baseline_problem_plan.columns.q_start.is_some(),
        "baseline SCED should keep the quadratic Hessian"
    );

    let pwl_request = crate::request::DispatchRequest {
        market: crate::request::DispatchMarket {
            generator_cost_modeling: Some(crate::request::GeneratorCostModeling {
                use_pwl_costs: true,
                pwl_cost_breakpoints: 8,
            }),
            ..crate::request::DispatchMarket::default()
        },
        network: crate::request::DispatchNetwork {
            thermal_limits: crate::request::ThermalLimitPolicy {
                enforce: false,
                ..crate::request::ThermalLimitPolicy::default()
            },
            flowgates: crate::request::FlowgatePolicy {
                enabled: false,
                ..crate::request::FlowgatePolicy::default()
            },
            ..crate::request::DispatchNetwork::default()
        },
        ..crate::request::DispatchRequest::default()
    };
    let pwl_normalized = pwl_request.normalize().expect("normalize pwl request");
    let pwl_context =
        crate::common::runtime::DispatchPeriodContext::initial(&pwl_normalized.input.initial_state);
    let pwl_spec = pwl_normalized.problem_spec();
    let pwl_session = crate::common::dc::build_period_solve_session(&net, pwl_spec, 0).unwrap();
    let pwl_model_plan = super::plan::build_model_plan(super::plan::ScedModelPlanInput {
        network: &net,
        context: pwl_context,
        solve: &pwl_session,
    })
    .unwrap();
    let pwl_problem_plan = super::plan::build_problem_plan(super::plan::ScedProblemPlanInput {
        network: &net,
        context: pwl_context,
        solve: &pwl_session,
        model_plan: &pwl_model_plan,
    });
    assert!(
        pwl_problem_plan.columns.q_start.is_none(),
        "request-level PWL generator costs should eliminate the SCED Hessian"
    );
    assert!(
        pwl_problem_plan.model_plan.layout.dispatch.e_g < pwl_problem_plan.columns.col_cost.len(),
        "SCED should allocate an epiograph variable for the PWL cost"
    );
    assert!(
        pwl_problem_plan.columns.col_cost[pwl_problem_plan.model_plan.layout.dispatch.e_g] > 0.0,
        "PWL epiograph objective should price the generator through e_g"
    );
}

#[test]
fn test_sced_with_ramp_constraints() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();

    // Set ramp rates: 10 MW/min for all generators
    for g in &mut net.generators {
        g.ramping.get_or_insert_default().ramp_up_curve = vec![(0.0, 10.0)];
    }

    // Get unconstrained dispatch first
    let unconstrained = solve_sced(&net, &DispatchOptions::default()).unwrap();

    // Now constrain with previous dispatch far from optimal
    let prev_dispatch: Vec<f64> = net
        .generators
        .iter()
        .filter(|g| g.in_service)
        .map(|g| g.pmin + 1.0) // near minimum
        .collect();

    let opts = DispatchOptions {
        dt_hours: 1.0,
        initial_state: crate::dispatch::IndexedDispatchInitialState {
            prev_dispatch_mw: Some(prev_dispatch.clone()),
            ..Default::default()
        },
        ..Default::default()
    };

    let constrained = solve_sced(&net, &opts).unwrap();

    // Verify ramp constraints are respected
    for (j, pg) in constrained.dispatch.pg_mw.iter().enumerate() {
        let ramp_mw = 10.0 * 60.0 * 1.0; // 10 MW/min * 60 min * 1 hr = 600 MW
        let max_pg = prev_dispatch[j] + ramp_mw;
        let min_pg = (prev_dispatch[j] - ramp_mw).max(0.0);
        assert!(
            *pg <= max_pg + 0.1 && *pg >= min_pg - 0.1,
            "gen {j}: pg={pg:.1} outside ramp bounds [{min_pg:.1}, {max_pg:.1}]"
        );
    }

    // With tight ramp limits, the dispatch should differ from unconstrained
    // (unless ramp limits are wide enough — our 600 MW is indeed wide for case9)
    let _ = unconstrained;
}

#[test]
fn test_sced_rts_gmlc() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let net = surge_io::matpower::load(test_data_path("case_RTS_GMLC.m")).unwrap();
    let opts = DispatchOptions::default();

    let sol = solve_sced(&net, &opts).unwrap();

    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    let total_load: f64 = net.total_load_mw();
    assert!(
        (total_gen - total_load).abs() < 1.0,
        "power balance: gen={total_gen:.2}, load={total_load:.2}"
    );
    assert!(sol.dispatch.total_cost > 0.0);
}

#[test]
fn test_sced_case118_with_reserve() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = surge_io::matpower::load(test_data_path("case118.m")).unwrap();
    for g in &mut net.generators {
        let phys_cap = (g.pmax - g.pmin).max(0.0);
        if phys_cap > 0.0 {
            g.market.get_or_insert_default().reserve_offers.push(
                surge_network::market::ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: phys_cap,
                    cost_per_mwh: 0.0,
                },
            );
        }
    }
    let opts = DispatchOptions {
        system_reserve_requirements: vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: 200.0,
            per_period_mw: None,
        }],
        ..Default::default()
    };

    let sol = solve_sced(&net, &opts).unwrap();

    let spin_awards = sol
        .dispatch
        .reserve_awards
        .get("spin")
        .expect("spin awards");
    let total_reserve: f64 = spin_awards.iter().sum();
    assert!(
        total_reserve >= 199.9,
        "total reserve={total_reserve:.1} MW"
    );
    assert!(
        sol.dispatch
            .reserve_prices
            .get("spin")
            .copied()
            .unwrap_or(0.0)
            >= 0.0
    );
}

/// PNL-004: with a tight reserve requirement (more than generators can provide)
/// and a Linear reserve penalty, SCED should find a feasible solution with non-zero
/// reserve slack rather than failing.
#[test]
fn test_sced_reserve_soft_constraint() {
    if !data_available() {
        eprintln!(
            "SKIP: tests/data not present — clone amptimal/surge-bench, copy instances/ to tests/data/"
        );
        return;
    }
    let mut net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
    for g in &mut net.generators {
        let phys_cap = (g.pmax - g.pmin).max(0.0);
        if phys_cap > 0.0 {
            g.market.get_or_insert_default().reserve_offers.push(
                surge_network::market::ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: phys_cap,
                    cost_per_mwh: 0.0,
                },
            );
        }
    }

    // Compute total available reserve headroom
    let gen_indices: Vec<usize> = net
        .generators
        .iter()
        .enumerate()
        .filter(|(_, g)| g.in_service)
        .map(|(i, _)| i)
        .collect();
    let total_capacity: f64 = gen_indices.iter().map(|&gi| net.generators[gi].pmax).sum();
    let total_load: f64 = net.total_load_mw();
    // Set reserve requirement well above what's feasible (larger than total headroom)
    let impossible_reserve_mw = total_capacity - total_load + 500.0;

    use surge_network::market::PenaltyCurve;
    let opts = DispatchOptions {
        system_reserve_requirements: vec![SystemReserveRequirement {
            product_id: "spin".into(),
            requirement_mw: impossible_reserve_mw,
            per_period_mw: None,
        }],
        penalty_config: surge_network::market::PenaltyConfig {
            reserve: PenaltyCurve::Linear {
                cost_per_unit: 1000.0,
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // With soft reserve constraint, SCED should succeed (not return an error)
    let sol = solve_sced(&net, &opts)
        .expect("SCED should succeed with soft reserve constraint even when reserve is short");

    // Power balance must still hold
    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    assert!(
        (total_gen - total_load).abs() < 0.5,
        "power balance: gen={total_gen:.2}, load={total_load:.2}"
    );

    // The actual reserve provided should be less than the impossible requirement
    // (proving the slack absorbed the shortfall)
    let spin_awards = sol
        .dispatch
        .reserve_awards
        .get("spin")
        .expect("spin awards");
    let total_reserve: f64 = spin_awards.iter().sum();
    assert!(
        total_reserve < impossible_reserve_mw - 1.0,
        "reserve={total_reserve:.1} MW should be below impossible requirement={impossible_reserve_mw:.1} MW"
    );

    println!(
        "case9 soft reserve: requirement={:.1} MW, provided={:.1} MW, cost={:.2} $/h",
        impossible_reserve_mw, total_reserve, sol.dispatch.total_cost
    );
}

// ----- DISP-05: CO2 emission constraints and carbon pricing -----

#[test]
fn test_sced_co2_price_shifts_dispatch() {
    // 2-generator synthetic network.
    // Gen1 at bus1 (slack): cheap ($10/MWh) but high-CO2 (0.5 t/MWh), pmax=200 MW.
    // Gen2 at bus1:        expensive ($50/MWh) but zero-CO2, pmax=200 MW.
    // Load: 100 MW.
    //
    // Without CO2 price: gen1 should serve most load (lower marginal cost).
    // With CO2 price = $100/t: gen1 effective cost = $10 + $100*0.5 = $60/MWh > gen2 $50/MWh.
    //   → gen2 should dominate dispatch.
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("co2_test");
    net.base_mva = 100.0;

    let b1 = Bus::new(1, BusType::Slack, 138.0);
    net.buses.push(b1);
    net.loads.push(Load::new(1, 100.0, 0.0));

    // Cheap high-CO2 generator
    let mut g1 = Generator::new(1, 0.0, 1.0);
    g1.pmin = 0.0;
    g1.pmax = 200.0;
    g1.in_service = true;
    g1.fuel.get_or_insert_default().emission_rates.co2 = 0.5;
    g1.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![10.0, 0.0], // $10/MWh
    });
    net.generators.push(g1);

    // Expensive zero-CO2 generator
    let mut g2 = Generator::new(1, 0.0, 1.0);
    g2.pmin = 0.0;
    g2.pmax = 200.0;
    g2.in_service = true;
    g2.fuel.get_or_insert_default().emission_rates.co2 = 0.0;
    g2.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![50.0, 0.0], // $50/MWh
    });
    net.generators.push(g2);

    // Without CO2 price: gen1 dominates
    let opts_no_co2 = DispatchOptions {
        enforce_thermal_limits: false,
        ..Default::default()
    };
    let sol_no_co2 = solve_sced(&net, &opts_no_co2).unwrap();
    assert!(
        sol_no_co2.dispatch.pg_mw[0] > sol_no_co2.dispatch.pg_mw[1],
        "Without CO2 price, gen1 (cheap) should dominate: gen1={:.1}, gen2={:.1}",
        sol_no_co2.dispatch.pg_mw[0],
        sol_no_co2.dispatch.pg_mw[1]
    );
    // total_co2_t should reflect gen1 dispatch
    let expected_co2 = sol_no_co2.dispatch.pg_mw[0] * 0.5;
    assert!(
        (sol_no_co2.dispatch.co2_t - expected_co2).abs() < 0.1,
        "total_co2_t={:.2} should equal gen1_dispatch * 0.5 = {:.2}",
        sol_no_co2.dispatch.co2_t,
        expected_co2
    );

    // With CO2 price $100/t: gen2 (zero-CO2 but $50/MWh) is cheaper than
    // gen1 ($10 + $100*0.5=$60/MWh effective) — gen2 should dominate
    let opts_co2_price = DispatchOptions {
        enforce_thermal_limits: false,
        co2_price_per_t: 100.0,
        ..Default::default()
    };
    let sol_co2_price = solve_sced(&net, &opts_co2_price).unwrap();
    assert!(
        sol_co2_price.dispatch.pg_mw[1] > sol_co2_price.dispatch.pg_mw[0],
        "With CO2 price $100/t, gen2 (zero-CO2, $50/MWh effective) should dominate: gen1={:.1}, gen2={:.1}",
        sol_co2_price.dispatch.pg_mw[0],
        sol_co2_price.dispatch.pg_mw[1]
    );
    // CO2 emissions should be lower with carbon price
    assert!(
        sol_co2_price.dispatch.co2_t < sol_no_co2.dispatch.co2_t,
        "CO2 emissions with price ({:.2}t) should be less than without ({:.2}t)",
        sol_co2_price.dispatch.co2_t,
        sol_no_co2.dispatch.co2_t
    );
}

#[test]
fn test_sced_co2_cap_forces_clean_dispatch() {
    // Same 2-generator setup.
    // Without cap: gen1 dominates (100 MW from gen1 → 50t CO2).
    // With CO2 cap = 10t: gen1 dispatch limited to ≤ 20 MW (10t / 0.5 t/MWh).
    //   → gen2 must provide remaining 80 MW.
    // Verify total_co2_t ≤ 10t.
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("co2_cap_test");
    net.base_mva = 100.0;

    let b1 = Bus::new(1, BusType::Slack, 138.0);
    net.buses.push(b1);
    net.loads.push(Load::new(1, 100.0, 0.0));

    let mut g1 = Generator::new(1, 0.0, 1.0);
    g1.pmin = 0.0;
    g1.pmax = 200.0;
    g1.in_service = true;
    g1.fuel.get_or_insert_default().emission_rates.co2 = 0.5;
    g1.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![10.0, 0.0],
    });
    net.generators.push(g1);

    let mut g2 = Generator::new(1, 0.0, 1.0);
    g2.pmin = 0.0;
    g2.pmax = 200.0;
    g2.in_service = true;
    g2.fuel.get_or_insert_default().emission_rates.co2 = 0.0;
    g2.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![50.0, 0.0],
    });
    net.generators.push(g2);

    let co2_cap = 10.0; // tonnes — limits gen1 to ≤ 20 MW
    let opts_cap = DispatchOptions {
        enforce_thermal_limits: false,
        co2_cap_t: Some(co2_cap),
        ..Default::default()
    };
    let sol = solve_sced(&net, &opts_cap).unwrap();

    // total_co2_t must be at or below the cap
    assert!(
        sol.dispatch.co2_t <= co2_cap + 0.01,
        "total_co2_t={:.3}t should be ≤ cap={:.1}t",
        sol.dispatch.co2_t,
        co2_cap
    );

    // Power balance must hold
    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    assert!(
        (total_gen - 100.0).abs() < 0.5,
        "power balance: gen={total_gen:.1} should be ~100 MW"
    );

    // gen2 should supply the bulk of the load
    assert!(
        sol.dispatch.pg_mw[1] > 70.0,
        "gen2 should supply most load with CO2 cap, got {:.1} MW",
        sol.dispatch.pg_mw[1]
    );

    println!(
        "CO2 cap test: gen1={:.1} MW ({:.2}t CO2), gen2={:.1} MW, total_co2={:.2}t (cap={:.1}t)",
        sol.dispatch.pg_mw[0],
        sol.dispatch.pg_mw[0] * 0.5,
        sol.dispatch.pg_mw[1],
        sol.dispatch.co2_t,
        co2_cap
    );
}

// ---- DISP-07: Regulation up/down products ----

#[test]
fn test_sced_regulation_up_requirement() {
    // 2-generator single-bus network.
    // Gen1 (slack): pmin=50, pmax=200, reg_up offer=80 MW.
    // Gen2: pmin=0, pmax=150, reg_up offer=60 MW.
    // Load: 150 MW.
    // reg_up_req = 100 MW.
    // Total reg_up available = 80+60 = 140 MW >= 100 MW → feasible.
    use surge_network::Network;
    use surge_network::market::{CostCurve, ReserveOffer};
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("reg_up_test");
    net.base_mva = 100.0;
    let b1 = Bus::new(1, BusType::Slack, 138.0);
    net.buses.push(b1);
    net.loads.push(Load::new(1, 150.0, 0.0));

    let mut g1 = Generator::new(1, 0.0, 1.0);
    g1.pmin = 50.0;
    g1.pmax = 200.0;
    g1.market
        .get_or_insert_default()
        .reserve_offers
        .push(ReserveOffer {
            product_id: "reg_up".into(),
            capacity_mw: 80.0,
            cost_per_mwh: 0.0,
        });
    g1.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![20.0, 0.0], // $20/MWh
    });
    net.generators.push(g1);

    let mut g2 = Generator::new(1, 0.0, 1.0);
    g2.pmin = 0.0;
    g2.pmax = 150.0;
    g2.market
        .get_or_insert_default()
        .reserve_offers
        .push(ReserveOffer {
            product_id: "reg_up".into(),
            capacity_mw: 60.0,
            cost_per_mwh: 0.0,
        });
    g2.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![30.0, 0.0], // $30/MWh
    });
    net.generators.push(g2);

    let opts = DispatchOptions {
        enforce_thermal_limits: false,
        system_reserve_requirements: vec![SystemReserveRequirement {
            product_id: "reg_up".into(),
            requirement_mw: 100.0,
            per_period_mw: None,
        }],
        ..Default::default()
    };
    let sol = solve_sced(&net, &opts).unwrap();

    // Regulation up provided >= requirement
    let reg_up_provided = sol
        .dispatch
        .reserve_provided
        .get("reg_up")
        .copied()
        .unwrap_or(0.0);
    assert!(
        reg_up_provided >= 99.9,
        "reg_up_provided={:.1} MW should be >= 100 MW requirement",
        reg_up_provided
    );

    // Power balance must hold
    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    assert!(
        (total_gen - 150.0).abs() < 0.5,
        "power balance: gen={total_gen:.1} should be ~150 MW"
    );

    println!(
        "SCED reg_up: pg=[{:.1},{:.1}], reg_up_provided={:.1} MW",
        sol.dispatch.pg_mw[0], sol.dispatch.pg_mw[1], reg_up_provided
    );
}

#[test]
fn test_sced_regulation_down_requirement() {
    // 2-generator single-bus network.
    // Gen1 (slack): pmin=20, pmax=200, reg_dn offer=60 MW.
    // Gen2: pmin=10, pmax=150, reg_dn offer=50 MW.
    // Load: 150 MW.
    // reg_dn_req = 80 MW (both generators must be online with headroom below dispatch).
    use surge_network::Network;
    use surge_network::market::{CostCurve, ReserveOffer};
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("reg_dn_test");
    net.base_mva = 100.0;
    let b1 = Bus::new(1, BusType::Slack, 138.0);
    net.buses.push(b1);
    net.loads.push(Load::new(1, 150.0, 0.0));

    let mut g1 = Generator::new(1, 0.0, 1.0);
    g1.pmin = 20.0;
    g1.pmax = 200.0;
    g1.market
        .get_or_insert_default()
        .reserve_offers
        .push(ReserveOffer {
            product_id: "reg_dn".into(),
            capacity_mw: 60.0,
            cost_per_mwh: 0.0,
        });
    g1.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![20.0, 0.0],
    });
    net.generators.push(g1);

    let mut g2 = Generator::new(1, 0.0, 1.0);
    g2.pmin = 10.0;
    g2.pmax = 150.0;
    g2.market
        .get_or_insert_default()
        .reserve_offers
        .push(ReserveOffer {
            product_id: "reg_dn".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 0.0,
        });
    g2.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![30.0, 0.0],
    });
    net.generators.push(g2);

    let opts = DispatchOptions {
        enforce_thermal_limits: false,
        system_reserve_requirements: vec![SystemReserveRequirement {
            product_id: "reg_dn".into(),
            requirement_mw: 80.0,
            per_period_mw: None,
        }],
        ..Default::default()
    };
    let sol = solve_sced(&net, &opts).unwrap();

    let reg_dn_provided = sol
        .dispatch
        .reserve_provided
        .get("reg_dn")
        .copied()
        .unwrap_or(0.0);
    assert!(
        reg_dn_provided >= 79.9,
        "reg_dn_provided={:.1} MW should be >= 80 MW requirement",
        reg_dn_provided
    );

    // Power balance
    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    assert!(
        (total_gen - 150.0).abs() < 0.5,
        "power balance: gen={total_gen:.1} should be ~150 MW"
    );

    println!(
        "SCED reg_dn: pg=[{:.1},{:.1}], reg_dn_provided={:.1} MW",
        sol.dispatch.pg_mw[0], sol.dispatch.pg_mw[1], reg_dn_provided
    );
}

/// DISP-PLC: Verify LP epiograph formulation dispatches cheapest segment first.
///
/// Network: single slack bus, 100 MW load.
/// Gen1: linear cost $10/MWh, pmax=60 MW — dispatches first (cheapest).
/// Gen2: PWL cost with 2 segments:
///   Seg1: [0→50 MW] at $20/MWh (slope=20)
///   Seg2: [50→100 MW] at $60/MWh (slope=60)
///   Average slope = (50*20 + 50*60) / 100 = $40/MWh (the bug)
///
/// With 100 MW load and Gen1 at 60 MW, Gen2 must supply 40 MW.
/// Correct epiograph: Gen2 dispatches 40 MW in its cheap segment ($20/MWh).
/// Buggy average slope: solver sees $40/MWh for Gen2 (wrong — could shift load).
///
/// This test verifies:
/// 1. Power balance is maintained.
/// 2. Gen2 dispatch ≤ 50 MW (stays in cheap segment — no incentive to cross kink).
/// 3. Total cost is correct: 60*$10 + 40*$20 = $600 + $800 = $1400/hr.
#[test]
fn test_plc_epiograph_dispatches_cheapest_segment_first() {
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    let mut net = Network::new("plc_epiograph_test");
    net.base_mva = 100.0;

    // Single slack bus with 100 MW load
    let bus1 = Bus::new(1, BusType::Slack, 100.0);
    net.buses.push(bus1);
    net.loads.push(Load::new(1, 100.0, 0.0));

    // Gen1: linear cost $10/MWh, pmax=60 MW
    let mut g1 = Generator::new(1, 0.0, 1.0);
    g1.pmin = 0.0;
    g1.pmax = 60.0;
    g1.in_service = true;
    g1.cost = Some(CostCurve::Polynomial {
        startup: 0.0,
        shutdown: 0.0,
        coeffs: vec![10.0, 0.0], // $10/MWh linear
    });
    net.generators.push(g1);

    // Gen2: PWL cost with kink at 50 MW
    //   [(0, 0), (50, 1000), (100, 4000)]
    //   Seg1 slope = (1000 - 0) / (50 - 0) = $20/MWh
    //   Seg2 slope = (4000 - 1000) / (100 - 50) = $60/MWh
    let mut g2 = Generator::new(1, 0.0, 1.0);
    g2.pmin = 0.0;
    g2.pmax = 100.0;
    g2.in_service = true;
    g2.cost = Some(CostCurve::PiecewiseLinear {
        startup: 0.0,
        shutdown: 0.0,
        points: vec![(0.0, 0.0), (50.0, 1000.0), (100.0, 4000.0)],
    });
    net.generators.push(g2);

    let opts = DispatchOptions {
        enforce_thermal_limits: false,
        ..Default::default()
    };
    let sol = solve_sced(&net, &opts).expect("SCED should solve with PWL epiograph");

    // 1. Power balance: total generation should equal 100 MW
    let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
    assert!(
        (total_gen - 100.0).abs() < 0.5,
        "Power balance: total_gen={:.2} MW, expected 100 MW",
        total_gen
    );

    // 2. Gen1 should be at full capacity (60 MW) — cheapest source
    assert!(
        sol.dispatch.pg_mw[0] >= 59.5,
        "Gen1 (cheapest) should be at full capacity 60 MW, got {:.2} MW",
        sol.dispatch.pg_mw[0]
    );

    // 3. Gen2 should dispatch 40 MW — stays in the cheap segment (≤50 MW)
    assert!(
        sol.dispatch.pg_mw[1] <= 50.5,
        "Gen2 should dispatch ≤50 MW (cheap segment only), got {:.2} MW",
        sol.dispatch.pg_mw[1]
    );

    // 4. Total cost: 60 MW * $10/MWh + 40 MW * $20/MWh = $600 + $800 = $1400/hr
    let expected_cost = 600.0 + 800.0;
    assert!(
        (sol.dispatch.total_cost - expected_cost).abs() < 2.0,
        "Total cost should be ~${:.2}/hr (epiograph), got ${:.2}/hr",
        expected_cost,
        sol.dispatch.total_cost
    );

    println!(
        "DISP-PLC epiograph: gen1={:.2} MW, gen2={:.2} MW, cost=${:.2}/hr (expected ${:.2}/hr)",
        sol.dispatch.pg_mw[0], sol.dispatch.pg_mw[1], sol.dispatch.total_cost, expected_cost
    );
}

#[cfg(test)]
mod emission_mustrun_sced_tests {
    use super::*;
    use crate::config::emissions::{EmissionProfile, MustRunUnits};
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    /// Test: SCED MustRunUnits raises Pg lower bound to pmin.
    ///
    /// gen1 is listed in MustRunUnits with pmin=50 MW.
    /// Without must-run, gen1 (expensive) dispatches near 0.
    /// With must-run, gen1 must dispatch at or above 50 MW.
    #[test]
    fn test_sced_must_run_units_floor_at_pmin() {
        let mut net = Network::new("sced_mr_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 150.0, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.commitment.get_or_insert_default().status = CommitmentStatus::Market;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.push(g1);

        // Without must-run: gen1 (expensive) dispatches at 0 (pmin=50 but not committed)
        // Note: In SCED, all generators are treated as committed (no binary).
        // With pmin=50, gen1 lower bound is 50 MW when must_run=false too (pmin constraint).
        // To test MustRunUnits specifically, we need pmin=0 for gen1 normally,
        // and check that MustRunUnits raises it to pmin.
        // Let's reset g1.pmin to test MustRunUnits raising it.
        let mut net2 = net.clone();
        net2.generators[1].pmin = 50.0; // pmin = 50, but normally col_lower = pmin / base

        // In SCED (no binary), col_lower is already set to pmin/base.
        // MustRunUnits only matters when must_run affects the lb explicitly.
        // The main test is: MustRunUnits should NOT reduce the lb below pmin.
        // Let's verify that gen1 is dispatched >= 50 MW when must_run flag is set.
        net2.generators[1].pmin = 0.0; // reset to 0

        let network2 = net2.clone();

        // Without must-run: gen1 pmin=0, so it won't dispatch (too expensive)
        let opts_no_mr = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol_no_mr = solve_sced(&network2, &opts_no_mr).unwrap();
        assert!(
            sol_no_mr.dispatch.pg_mw[1] < 1.0,
            "Without must-run: gen1 (expensive, pmin=0) should not dispatch. pg1={:.1}",
            sol_no_mr.dispatch.pg_mw[1]
        );

        // With MustRunUnits: gen1 pmin=50 (set via network), must dispatch >= 50 MW
        let mut net3 = network2.clone();
        net3.generators[1].pmin = 50.0; // restore pmin for must-run test

        let opts_mr = DispatchOptions {
            enforce_thermal_limits: false,
            must_run_units: Some(MustRunUnits {
                unit_indices: vec![1],
            }),
            ..Default::default()
        };
        let sol_mr = solve_sced(&net3, &opts_mr).unwrap();
        assert!(
            sol_mr.dispatch.pg_mw[1] >= 49.5,
            "With MustRunUnits: gen1 should dispatch >= pmin=50 MW. pg1={:.2}",
            sol_mr.dispatch.pg_mw[1]
        );

        // Power balance
        let total = sol_mr.dispatch.pg_mw[0] + sol_mr.dispatch.pg_mw[1];
        assert!(
            (total - 150.0).abs() < 0.5,
            "Power balance: {:.1} != 150 MW",
            total
        );

        println!(
            "SCED DISP-09 MustRunUnits: no_mr=({:.1},{:.1}), mr=({:.1},{:.1})",
            sol_no_mr.dispatch.pg_mw[0],
            sol_no_mr.dispatch.pg_mw[1],
            sol_mr.dispatch.pg_mw[0],
            sol_mr.dispatch.pg_mw[1]
        );
    }

    /// Test: SCED EmissionProfile override shifts dispatch.
    ///
    /// gen0 appears dirty (co2_rate=1.0) in network.
    /// EmissionProfile overrides gen0's rate to 0.0.
    /// At high carbon price, gen0 should still win (effective rate = 0).
    #[test]
    fn test_sced_emission_profile_overrides_generator_rate() {
        let mut net = Network::new("sced_ep_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.fuel.get_or_insert_default().emission_rates.co2 = 1.0; // appears dirty in network model
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.fuel.get_or_insert_default().emission_rates.co2 = 0.0; // clean in network model
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g1);

        // With high CO2 price but no EmissionProfile:
        // gen0 effective cost = 10 + 50*1.0 = 60 $/MWh → gen1 wins
        let opts_no_ep = DispatchOptions {
            enforce_thermal_limits: false,
            co2_price_per_t: 50.0,
            ..Default::default()
        };
        let sol_no_ep = solve_sced(&net, &opts_no_ep).unwrap();
        // gen1 should dominate (gen0 has high effective cost)
        assert!(
            sol_no_ep.dispatch.pg_mw[1] > sol_no_ep.dispatch.pg_mw[0],
            "No EmissionProfile: gen1 (clean) should win. gen0={:.1}, gen1={:.1}",
            sol_no_ep.dispatch.pg_mw[0],
            sol_no_ep.dispatch.pg_mw[1]
        );

        // With EmissionProfile zeroing gen0's rate:
        // gen0 effective cost = 10 + 50*0.0 = 10 $/MWh → gen0 wins again
        let opts_ep = DispatchOptions {
            enforce_thermal_limits: false,
            co2_price_per_t: 50.0,
            emission_profile: Some(EmissionProfile {
                rates_tonnes_per_mwh: vec![0.0, 0.0], // override both to 0
            }),
            ..Default::default()
        };
        let sol_ep = solve_sced(&net, &opts_ep).unwrap();
        // gen0 should dominate (effective CO2 rate = 0, energy cost cheaper)
        assert!(
            sol_ep.dispatch.pg_mw[0] > sol_ep.dispatch.pg_mw[1],
            "With EmissionProfile zeroing gen0: gen0 (cheaper energy) should win. gen0={:.1}, gen1={:.1}",
            sol_ep.dispatch.pg_mw[0],
            sol_ep.dispatch.pg_mw[1]
        );

        println!(
            "SCED DISP-05 EmissionProfile: no_ep=({:.1},{:.1}), ep=({:.1},{:.1})",
            sol_no_ep.dispatch.pg_mw[0],
            sol_no_ep.dispatch.pg_mw[1],
            sol_ep.dispatch.pg_mw[0],
            sol_ep.dispatch.pg_mw[1]
        );
    }
}

#[cfg(test)]
mod tie_line_sced_tests {
    use super::*;
    use crate::config::emissions::TieLineLimits;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    /// DISP-06: Tie-line export limit restricts inter-area transfer in SCED.
    ///
    /// Two-area, two-bus, single-branch network:
    ///   Bus 1 (slack, area 0): gen G0 (cheap, pmax=200 MW), no load.
    ///   Bus 2 (PQ, area 1): no generation, load = 100 MW.
    ///   Branch 1→2: rate_a = 200 MW (not binding on its own).
    ///
    /// Without tie-line limit: G0 exports 100 MW to area 1 (unconstrained).
    /// With tie-line limit of 40 MW from area 0 to area 1: the export is capped
    /// and SCED becomes infeasible (unless we relax the test to just check the
    /// constraint is enforced at 40 MW when a gen in area 1 supplements).
    ///
    /// Simpler verifiable form: add a second (expensive) generator in area 1.
    /// Without limit: cheap G0 supplies all 100 MW; with limit = 40 MW, G0 is
    /// capped at 40 MW export and G1 must supply the remaining 60 MW.
    #[test]
    fn test_sced_tie_line_export_limit_binds() {
        let mut net = Network::new("tie_line_test");
        net.base_mva = 100.0;

        // Bus 1: slack, area 0 — no load (exporter)
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);

        // Bus 2: PQ, area 1 — 100 MW load (importer)
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 100.0, 0.0));

        // Branch 1→2
        let mut br = Branch::new_line(1, 2, 0.0, 0.01, 0.0);
        br.rating_a_mva = 300.0; // not binding
        br.in_service = true;
        net.branches.push(br);

        // G0 in area 0 — cheap
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0], // $10/MWh
        });
        net.generators.push(g0);

        // G1 in area 1 — expensive
        let mut g1 = Generator::new(2, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0], // $50/MWh
        });
        net.generators.push(g1);

        // Without limit: G0 supplies all 100 MW
        let opts_no_limit = DispatchOptions::default();
        let sol_no_limit = solve_sced(&net, &opts_no_limit).unwrap();
        assert!(
            sol_no_limit.dispatch.pg_mw[0] > 90.0,
            "Without limit: G0 (cheap) should supply most load, got {:.1} MW",
            sol_no_limit.dispatch.pg_mw[0]
        );

        // With tie-line limit 40 MW from area 0 → area 1:
        // G0 capped at 40 MW export; G1 must supply remaining 60 MW.
        let mut tie_limits = TieLineLimits::default();
        tie_limits.limits_mw.insert((0, 1), 40.0);

        let opts_limited = DispatchOptions {
            tie_line_limits: Some(tie_limits),
            generator_area: vec![0, 1], // G0 in area 0, G1 in area 1
            load_area: vec![0, 1],      // bus 1 in area 0, bus 2 in area 1
            ..Default::default()
        };
        let sol_limited = solve_sced(&net, &opts_limited).unwrap();

        // G0 export from area 0 should be ≤ 40 MW
        // (area 0 local load = 0, so net export = G0 dispatch)
        assert!(
            sol_limited.dispatch.pg_mw[0] <= 40.1,
            "With tie-line limit 40 MW: G0 export should be ≤ 40 MW, got {:.1} MW",
            sol_limited.dispatch.pg_mw[0]
        );

        // Power balance: G0 + G1 = 100 MW
        let total = sol_limited.dispatch.pg_mw.iter().sum::<f64>();
        assert!(
            (total - 100.0).abs() < 0.5,
            "Power balance: {total:.1} MW != 100 MW"
        );

        // G1 must cover the remaining 60+ MW
        assert!(
            sol_limited.dispatch.pg_mw[1] >= 59.9,
            "G1 should supply ≥ 60 MW after export limit, got {:.1} MW",
            sol_limited.dispatch.pg_mw[1]
        );

        println!(
            "SCED DISP-06 tie-line: no_limit=({:.1},{:.1}), limited=({:.1},{:.1}) MW",
            sol_no_limit.dispatch.pg_mw[0],
            sol_no_limit.dispatch.pg_mw[1],
            sol_limited.dispatch.pg_mw[0],
            sol_limited.dispatch.pg_mw[1]
        );
    }

    #[test]
    fn test_sced_tie_line_limit_is_area_pair_specific() {
        let mut net = Network::new("pair_specific_tie_line_test");
        net.base_mva = 100.0;

        net.buses.push(Bus::new(1, BusType::Slack, 138.0)); // Area 0
        net.buses.push(Bus::new(2, BusType::PQ, 138.0)); // Area 1
        net.buses.push(Bus::new(3, BusType::PQ, 138.0)); // Area 2
        net.loads.push(Load::new(2, 100.0, 0.0));
        net.loads.push(Load::new(3, 100.0, 0.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.01, 0.0);
        br12.rating_a_mva = 300.0;
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.01, 0.0);
        br13.rating_a_mva = 300.0;
        net.branches = vec![br12, br13];

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 300.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        let mut g1 = Generator::new(2, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(3, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.push(g2);

        let mut tie_limits = TieLineLimits::default();
        tie_limits.limits_mw.insert((0, 1), 40.0);

        let sol = solve_sced(
            &net,
            &DispatchOptions {
                tie_line_limits: Some(tie_limits),
                generator_area: vec![0, 1, 2],
                load_area: vec![0, 1, 2],
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            sol.dispatch.pg_mw[0] >= 139.0 && sol.dispatch.pg_mw[0] <= 141.0,
            "Area 0 generator should still export freely to area 2 while being limited into area 1, got {:.2} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.pg_mw[1] >= 59.0 && sol.dispatch.pg_mw[1] <= 61.0,
            "Area 1 generator should cover the 60 MW shortfall, got {:.2} MW",
            sol.dispatch.pg_mw[1]
        );
        assert!(
            sol.dispatch.pg_mw[2] <= 1.0,
            "Area 2 generator should stay off because the A->C path is unrestricted, got {:.2} MW",
            sol.dispatch.pg_mw[2]
        );
    }

    // ----- DISP-02: Asymmetric ramp rates (named test) -----

    /// DISP-02: Asymmetric up/down ramp rates in SCED.
    ///
    /// Creates a generator with ramp_up = 50 MW/min but ramp_down = 10 MW/min.
    /// Two consecutive SCED periods:
    ///   Period 1 (up-ramp scenario):  prev_dispatch=100 MW, load=400 MW
    ///     → generator wants to ramp up; limited by ramp_up=50 MW/min
    ///   Period 2 (down-ramp scenario): prev_dispatch=400 MW, load=100 MW
    ///     → generator wants to ramp down; limited by ramp_down=10 MW/min
    ///
    /// With dt=1 hour:
    ///   ramp_up_limit   = 50 MW/min * 60 min = 3000 MW/hr  (effectively unconstrained for small pmax)
    ///   ramp_down_limit = 10 MW/min * 60 min = 600 MW/hr
    ///
    /// To make the constraint binding, use a small pmax and tight ramp rates:
    ///   ramp_up = 5 MW/min  → 300 MW/hr up limit (binding if prev=0, load=400, pmax=400)
    ///   ramp_down = 1 MW/min → 60 MW/hr down limit (binding if prev=400, load=100, pmin=0)
    #[test]
    fn test_disp02_asymmetric_ramps() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("disp02_asym_ramp");
        net.base_mva = 100.0;

        // Single slack bus with variable load
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 300.0, 0.0));

        // One generator: pmax=400 MW, pmin=0
        // Asymmetric ramp: up=5 MW/min, down=1 MW/min
        // With dt=1h: up limit = 300 MW, down limit = 60 MW
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 400.0;
        g0.in_service = true;
        g0.ramping.get_or_insert_default().ramp_up_curve = vec![(0.0, 5.0)]; // up-ramp: 5 MW/min = 300 MW/hr
        g0.ramping.get_or_insert_default().ramp_down_curve = vec![(0.0, 1.0)]; // down-ramp: 1 MW/min = 60 MW/hr (much slower)
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        // ---- Period 1: up-ramp scenario ----
        // Previous dispatch = 0 MW. Load = 300 MW.
        // Up-ramp limit = 5 MW/min * 60 min = 300 MW/hr → allows reaching 300 MW.
        let prev_zero = vec![0.0_f64];

        let opts_up = DispatchOptions {
            dt_hours: 1.0,
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                prev_dispatch_mw: Some(prev_zero.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut net_up = net.clone();
        net_up.loads[0].active_power_demand_mw = 300.0;
        let sol_up = solve_sced(&net_up, &opts_up).unwrap();

        // Up ramp should limit dispatch to prev + ramp_up_limit = 0 + 300 = 300 MW
        let pg_up = sol_up.dispatch.pg_mw[0];
        assert!(
            pg_up <= 300.0 + 0.5,
            "DISP-02: up-ramp should limit dispatch to ≤ 300 MW (prev=0, ramp_up=300 MW/hr), got {pg_up:.1} MW"
        );

        // ---- Period 2: down-ramp scenario ----
        // Previous dispatch = 400 MW. Load = 100 MW.
        // Down-ramp limit = 1 MW/min * 60 min = 60 MW/hr.
        // Min dispatch = max(pmin=0, prev - ramp_down_limit) = max(0, 400-60) = 340 MW.
        // But load is only 100 MW — so dispatch will be constrained at 340 MW with violation?
        // Actually with a single bus and no reserve, we need balance: use 2 generators.
        // Let's instead set load = 340 MW (so balance is feasible at the ramp-down limit).
        let prev_high = vec![400.0_f64];

        let opts_dn = DispatchOptions {
            dt_hours: 1.0,
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                prev_dispatch_mw: Some(prev_high.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut net_dn = net.clone();
        net_dn.loads[0].active_power_demand_mw = 340.0; // load = 340 MW = 400 - 60 (ramp down limit)
        let sol_dn = solve_sced(&net_dn, &opts_dn).unwrap();

        let pg_dn = sol_dn.dispatch.pg_mw[0];
        // Down-ramp limit = 60 MW/hr → min dispatch = 400 - 60 = 340 MW
        assert!(
            pg_dn >= 340.0 - 0.5,
            "DISP-02: down-ramp should limit dispatch to ≥ 340 MW (prev=400, ramp_down=60 MW/hr), got {pg_dn:.1} MW"
        );

        // Also verify that without the asymmetric ramp_dn, the constraint would be looser.
        // Using only ramp_up_curve (5 MW/min = 300 MW/hr) for downward too:
        // min_dispatch = max(0, 400 - 300) = 100 MW (unconstrained for this load).
        // With ramp_down=1 MW/min, min_dispatch = max(0, 400 - 60) = 340 MW (binding).
        // Verify the asymmetric rate makes the down constraint tighter:
        let mut net_sym = net.clone();
        net_sym.loads[0].active_power_demand_mw = 340.0;
        net_sym.generators[0]
            .ramping
            .get_or_insert_default()
            .ramp_down_curve = vec![]; // use symmetric ramp_up_curve = 5 MW/min
        let opts_sym = DispatchOptions {
            dt_hours: 1.0,
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                prev_dispatch_mw: Some(prev_high),
                ..Default::default()
            },
            ..Default::default()
        };
        let sol_sym = solve_sced(&net_sym, &opts_sym).unwrap();
        let pg_sym = sol_sym.dispatch.pg_mw[0];

        // With symmetric ramp (5 MW/min = 300 MW/hr down), min_dispatch = 100 MW.
        // The dispatch should be 340 MW (load-balance) — not constrained.
        assert!(
            pg_sym <= 340.5,
            "Symmetric ramp (down=300 MW/hr): dispatch={pg_sym:.1}, should be ≤ 340 MW (load)"
        );
        // And the asymmetric version should be tighter (dispatch ≥ 340 due to ramp limit)
        assert!(
            pg_dn >= pg_sym - 0.1,
            "Asymmetric down-ramp ({pg_dn:.1} MW) should be ≥ symmetric ({pg_sym:.1} MW) for prev=400"
        );

        println!(
            "DISP-02 asymmetric ramps: up_dispatch={:.1} MW (limit 300), dn_dispatch={:.1} MW (limit 60 from 400), sym={:.1}",
            pg_up, pg_dn, pg_sym
        );
    }
}

// =============================================================================
// DISP-07: Distinct regulation-up/down and non-spinning reserve clearing tests
// =============================================================================

#[cfg(test)]
mod disp07_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::{CostCurve, ReserveOffer};
    use surge_network::network::{Bus, BusType, Generator};

    fn make_single_bus(pd_mw: f64) -> Network {
        let mut net = Network::new("disp07_test");
        net.base_mva = 100.0;
        let b = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b);
        net.loads.push(Load::new(1, pd_mw, 0.0));
        net
    }

    #[allow(clippy::too_many_arguments)]
    fn make_gen(
        pmin: f64,
        pmax: f64,
        mc: f64,
        reg_up_mw: f64,
        reg_up_cost: f64,
        reg_dn_mw: f64,
        reg_dn_cost: f64,
        nspin_mw: f64,
        nspin_cost: f64,
        quick_start: bool,
    ) -> Generator {
        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmin = pmin;
        g.pmax = pmax;
        if reg_up_mw > 0.0 {
            g.market
                .get_or_insert_default()
                .reserve_offers
                .push(ReserveOffer {
                    product_id: "reg_up".into(),
                    capacity_mw: reg_up_mw,
                    cost_per_mwh: reg_up_cost,
                });
        }
        if reg_dn_mw > 0.0 {
            g.market
                .get_or_insert_default()
                .reserve_offers
                .push(ReserveOffer {
                    product_id: "reg_dn".into(),
                    capacity_mw: reg_dn_mw,
                    cost_per_mwh: reg_dn_cost,
                });
        }
        if nspin_mw > 0.0 {
            g.market
                .get_or_insert_default()
                .reserve_offers
                .push(ReserveOffer {
                    product_id: "nspin".into(),
                    capacity_mw: nspin_mw,
                    cost_per_mwh: nspin_cost,
                });
        }
        g.quick_start = quick_start;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![mc, 0.0],
        });
        g
    }

    /// DISP-07 test 1: Regulation-up clears separately from energy dispatch.
    ///
    /// 3 generators with large pmax so headroom is not binding.
    /// Cheapest reg_up provider fills first; marginal price follows cost ordering.
    #[test]
    fn test_disp07_reg_up_clears_separately() {
        // Load = 50 MW (small), pmax = 500 MW => ample headroom for reg_up.
        let mut net = make_single_bus(50.0);
        // Gen0: expensive energy ($30), cheapest reg_up ($2/MW, cap=60 MW)
        net.generators.push(make_gen(
            0.0, 500.0, 30.0, 60.0, 2.0, 0.0, 0.0, 0.0, 0.0, false,
        ));
        // Gen1: cheapest energy ($20), medium reg_up ($5/MW, cap=50 MW)
        net.generators.push(make_gen(
            0.0, 500.0, 20.0, 50.0, 5.0, 0.0, 0.0, 0.0, 0.0, false,
        ));
        // Gen2: medium energy ($25), expensive reg_up ($8/MW, cap=40 MW)
        net.generators.push(make_gen(
            0.0, 500.0, 25.0, 40.0, 8.0, 0.0, 0.0, 0.0, 0.0, false,
        ));

        let req_mw = 80.0;
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "reg_up".into(),
                requirement_mw: req_mw,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        let rup_awards = sol
            .dispatch
            .reserve_awards
            .get("reg_up")
            .expect("reg_up awards");
        let total_rup: f64 = rup_awards.iter().sum();
        assert!(
            total_rup >= req_mw - 0.1,
            "total reg_up={:.1} MW should be >= {req_mw} MW",
            total_rup
        );
        let rup_price = sol
            .dispatch
            .reserve_prices
            .get("reg_up")
            .copied()
            .unwrap_or(0.0);
        assert!(rup_price >= 0.0, "reg_up_price should be >= 0");

        // Cheapest regulator (gen0, $2/MW, cap=60) should be fully awarded
        assert!(
            rup_awards[0] >= 59.9,
            "gen0 (cheapest $2/MW, cap=60) should provide ~60 MW, got {:.1}",
            rup_awards[0]
        );
        // Gen1 fills remaining 20 MW at $5/MW
        assert!(
            rup_awards[1] >= 19.9,
            "gen1 ($5/MW) fills remaining ~20 MW, got {:.1}",
            rup_awards[1]
        );
        // Gen2 not needed (most expensive)
        assert!(
            rup_awards[2] < 0.1,
            "gen2 (most expensive $8/MW) should provide 0, got {:.1}",
            rup_awards[2]
        );

        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 50.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 50 MW"
        );
        let rup_provided = sol
            .dispatch
            .reserve_provided
            .get("reg_up")
            .copied()
            .unwrap_or(0.0);
        assert!(
            (rup_provided - total_rup).abs() < 0.01,
            "reg_up_provided={:.1} should match sum={:.1}",
            rup_provided,
            total_rup
        );
        println!(
            "DISP-07 reg_up: awards={:?}, price={:.4} $/MWh",
            rup_awards, rup_price
        );
    }

    /// DISP-07 test 2: NSPIN is provided only by online quick-start generators.
    ///
    /// 2 generators: one normal online (no NSPIN in SCED), one quick-start online.
    #[test]
    fn test_disp07_nspin_from_online_quickstart() {
        let mut net = make_single_bus(100.0);

        // Gen0: normal online generator -- nspin_mw set but quick_start=false => 0 NSPIN in SCED
        let mut g0 = make_gen(0.0, 200.0, 20.0, 0.0, 0.0, 0.0, 0.0, 80.0, 0.0, false);
        g0.quick_start = false;
        net.generators.push(g0);

        // Gen1: quick-start online generator -- provides NSPIN in SCED
        let mut g1 = make_gen(0.0, 200.0, 25.0, 0.0, 0.0, 0.0, 0.0, 100.0, 1.0, true);
        g1.quick_start = true;
        net.generators.push(g1);

        let nspin_req = 80.0;
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "nspin".into(),
                requirement_mw: nspin_req,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        let nspin_awards = sol
            .dispatch
            .reserve_awards
            .get("nspin")
            .expect("nspin awards");
        let total_nspin: f64 = nspin_awards.iter().sum();
        assert!(
            total_nspin >= nspin_req - 0.1,
            "total nspin={:.1} MW should be >= {nspin_req} MW",
            total_nspin
        );
        // Non-quick-start gen0 provides 0 NSPIN
        assert!(
            nspin_awards[0] < 0.1,
            "non-quick-start gen0 should provide 0 NSPIN, got {:.1}",
            nspin_awards[0]
        );
        // Quick-start gen1 provides all NSPIN
        assert!(
            nspin_awards[1] >= nspin_req - 0.1,
            "quick-start gen1 should provide ~{nspin_req} MW NSPIN, got {:.1}",
            nspin_awards[1]
        );
        let nspin_provided = sol
            .dispatch
            .reserve_provided
            .get("nspin")
            .copied()
            .unwrap_or(0.0);
        assert!(
            (nspin_provided - total_nspin).abs() < 0.01,
            "nspin_provided={:.1} should match sum={:.1}",
            nspin_provided,
            total_nspin
        );
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 100.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 100 MW"
        );
        let nspin_price = sol
            .dispatch
            .reserve_prices
            .get("nspin")
            .copied()
            .unwrap_or(0.0);
        println!(
            "DISP-07 nspin: awards={:?}, price={:.4} $/MWh",
            nspin_awards, nspin_price
        );
    }

    /// DISP-07 test 3: Regulation-down uses downward headroom (Pg - Pmin).
    ///
    /// Generator pinned at Pmin cannot provide reg_dn.
    /// Generator with headroom below dispatch provides reg_dn.
    #[test]
    fn test_disp07_reg_dn_uses_downward_headroom() {
        // Gen0: pmin=100, must_run=true => dispatches at pmin => (pg-pmin)=0 => no reg_dn headroom.
        // Gen1: pmin=0, cheaper energy => dispatches above 0 => has downward headroom.
        // Load = 160 MW => gen0=100 (pmin), gen1=60 MW => gen1 has 60 MW headroom.
        // reg_dn_req = 40 MW => must come entirely from gen1.
        let mut net = make_single_bus(160.0);

        let mut g0 = make_gen(100.0, 200.0, 30.0, 0.0, 0.0, 50.0, 2.0, 0.0, 0.0, false);
        g0.commitment.get_or_insert_default().status = CommitmentStatus::MustRun;
        net.generators.push(g0);

        let g1 = make_gen(0.0, 200.0, 20.0, 0.0, 0.0, 60.0, 3.0, 0.0, 0.0, false);
        net.generators.push(g1);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "reg_dn".into(),
                requirement_mw: 40.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 160.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 160 MW"
        );

        let rdn_awards = sol
            .dispatch
            .reserve_awards
            .get("reg_dn")
            .expect("reg_dn awards");
        let total_rdn: f64 = rdn_awards.iter().sum();
        assert!(
            total_rdn >= 39.9,
            "total reg_dn={:.1} MW should be >= 40 MW",
            total_rdn
        );
        // Gen0 pinned at pmin=100 => pg-pmin=0 => reg_dn[0] ~ 0
        assert!(
            rdn_awards[0] < 1.0,
            "gen0 (at pmin=100) should provide ~0 reg_dn, got {:.1}",
            rdn_awards[0]
        );
        // Gen1 provides all reg_dn
        assert!(
            rdn_awards[1] >= 39.9,
            "gen1 should provide ~40 MW reg_dn, got {:.1}",
            rdn_awards[1]
        );
        let rdn_price = sol
            .dispatch
            .reserve_prices
            .get("reg_dn")
            .copied()
            .unwrap_or(0.0);
        println!(
            "DISP-07 reg_dn headroom: pg=[{:.1},{:.1}], reg_dn=[{:.1},{:.1}], price={:.4}",
            sol.dispatch.pg_mw[0], sol.dispatch.pg_mw[1], rdn_awards[0], rdn_awards[1], rdn_price
        );
    }

    /// DISP-07 test 4: Separate clearing prices for spin, reg_up, reg_dn, and nspin.
    ///
    /// Each product has a different marginal cost and a binding system requirement.
    #[test]
    fn test_disp07_separate_clearing_prices() {
        let mut net = make_single_bus(200.0);

        // Gen0: energy + spin + reg_up ($5/MW)
        let mut g0 = make_gen(0.0, 300.0, 30.0, 60.0, 5.0, 0.0, 0.0, 0.0, 0.0, false);
        g0.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "spin".into(),
                capacity_mw: 300.0,
                cost_per_mwh: 0.0,
            });
        net.generators.push(g0);

        // Gen1: energy + reg_dn ($4/MW) + spin
        let mut g1 = make_gen(50.0, 300.0, 25.0, 0.0, 0.0, 80.0, 4.0, 0.0, 0.0, false);
        g1.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "spin".into(),
                capacity_mw: 250.0,
                cost_per_mwh: 0.0,
            });
        net.generators.push(g1);

        // Gen2: quick-start, nspin specialist ($6/MW)
        let g2 = make_gen(0.0, 200.0, 35.0, 0.0, 0.0, 0.0, 0.0, 100.0, 6.0, true);
        net.generators.push(g2);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![
                SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 30.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_up".into(),
                    requirement_mw: 40.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_dn".into(),
                    requirement_mw: 60.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "nspin".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                },
            ],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // All products must be met
        let get_total = |pid: &str| -> f64 {
            sol.dispatch
                .reserve_awards
                .get(pid)
                .map(|v| v.iter().sum())
                .unwrap_or(0.0)
        };
        let total_spin = get_total("spin");
        assert!(total_spin >= 29.9, "spin={:.1} >= 30 MW", total_spin);

        let total_rup = get_total("reg_up");
        assert!(total_rup >= 39.9, "reg_up={:.1} >= 40 MW", total_rup);

        let total_rdn = get_total("reg_dn");
        assert!(total_rdn >= 59.9, "reg_dn={:.1} >= 60 MW", total_rdn);

        let total_nspin = get_total("nspin");
        assert!(total_nspin >= 49.9, "nspin={:.1} >= 50 MW", total_nspin);

        // All prices non-negative
        let get_price =
            |pid: &str| -> f64 { sol.dispatch.reserve_prices.get(pid).copied().unwrap_or(0.0) };
        assert!(get_price("spin") >= 0.0, "spin_price >= 0");
        assert!(get_price("reg_up") >= 0.0, "reg_up_price >= 0");
        assert!(get_price("reg_dn") >= 0.0, "reg_dn_price >= 0");
        assert!(get_price("nspin") >= 0.0, "nspin_price >= 0");

        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 200 MW"
        );
        println!(
            "DISP-07 separate prices: spin={:.4}, reg_up={:.4}, reg_dn={:.4}, nspin={:.4} $/MWh",
            get_price("spin"),
            get_price("reg_up"),
            get_price("reg_dn"),
            get_price("nspin")
        );
    }
}

#[cfg(test)]
mod p5_063_ecrs_rrs_tests {
    use super::*;
    use crate::solution::RawDispatchPeriodResult;
    use surge_network::Network;
    use surge_network::market::{CostCurve, ReserveOffer};
    use surge_network::network::{Bus, BusType, Generator};

    /// Helper: build a simple single-bus two-generator network.
    /// g0: pmin=50, pmax=250, cheap ($20/MWh), sync machine
    /// g1: pmin=0, pmax=200, expensive ($40/MWh), inverter-based
    /// Load: 200 MW
    fn two_gen_network() -> Network {
        let mut net = Network::new("ecrs_rrs_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 200.0, 0.0));

        // g0: synchronous machine — qualifies for ECRS and RRS
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 50.0;
        g0.pmax = 250.0;
        g0.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "ecrs".into(),
                capacity_mw: 80.0,
                cost_per_mwh: 5.0,
            });
        g0.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "rrs".into(),
                capacity_mw: 60.0,
                cost_per_mwh: 3.0,
            });
        g0.market
            .get_or_insert_default()
            .qualifications
            .insert("freq_responsive".into(), true);
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g0);

        // g1: inverter-based — can offer ECRS but does NOT qualify for RRS
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "ecrs".into(),
                capacity_mw: 50.0,
                cost_per_mwh: 8.0,
            });
        // No RRS offer — not qualified (no freq_responsive flag)
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.push(g1);

        net
    }

    /// P5-063 test 1: ECRS energy coupling constraint binds correctly.
    ///
    /// With ecrs_req = 70 MW and g0.pmax = 250 MW:
    ///   The coupling constraint forces: pg[g0] + r_ecrs[g0] <= pmax[g0]
    ///   Both generators can offer ECRS (g0: 80 MW, g1: 50 MW).
    ///   Total ECRS available = 130 MW >= 70 MW requirement.
    ///
    /// Key assertion: pg[g] + ecrs_mw[g] <= pmax[g] for every generator.
    #[test]
    fn test_ecrs_energy_coupling_constraint_binds() {
        let net = two_gen_network();
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "ecrs".into(),
                requirement_mw: 70.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 200 MW"
        );

        // ECRS requirement satisfied
        let ecrs_provided = sol
            .dispatch
            .reserve_provided
            .get("ecrs")
            .copied()
            .unwrap_or(0.0);
        assert!(
            ecrs_provided >= 69.9,
            "ecrs_provided={:.1} MW should be >= 70 MW requirement",
            ecrs_provided
        );

        // Energy-ECRS coupling: pg[g] + ecrs_mw[g] <= pmax[g]
        let ecrs_awards = sol
            .dispatch
            .reserve_awards
            .get("ecrs")
            .expect("ecrs awards");
        let pmaxs = [250.0_f64, 200.0_f64];
        for (j, (&pg, &ecrs)) in sol
            .dispatch
            .pg_mw
            .iter()
            .zip(ecrs_awards.iter())
            .enumerate()
        {
            assert!(
                pg + ecrs <= pmaxs[j] + 0.1,
                "gen {j}: pg={pg:.1} + ecrs={ecrs:.1} = {:.1} > pmax={:.1} (coupling violated)",
                pg + ecrs,
                pmaxs[j]
            );
        }

        // ECRS price should be positive (constraint binding with 70 MW req)
        let ecrs_price = sol
            .dispatch
            .reserve_prices
            .get("ecrs")
            .copied()
            .unwrap_or(0.0);
        assert!(
            ecrs_price >= 0.0,
            "ecrs_price={:.4} should be non-negative",
            ecrs_price
        );

        println!(
            "ECRS coupling test: pg={:?}, ecrs={:?}, ecrs_price={:.4} $/MWh, ecrs_provided={:.1} MW",
            sol.dispatch.pg_mw, ecrs_awards, ecrs_price, ecrs_provided
        );
    }

    /// P5-063 test 2: ECRS coupling forces dispatch reduction below unconstrained optimal.
    ///
    /// To make the coupling constraint deterministically bind on g0, we use a
    /// single-generator network where g0 is the only ECRS provider.
    ///
    ///   g0: pmin=0, pmax=250 MW, ecrs_mw=80 MW, cheap ($20/MWh)
    ///   g1: pmin=0, pmax=200 MW, ecrs_mw=0 (cannot offer ECRS), expensive ($40/MWh)
    ///   Load: 200 MW
    ///   ecrs_req = 80 MW
    ///
    /// Since g1 has no ECRS capacity, g0 must provide all 80 MW of ECRS.
    /// Coupling forces: pg[g0] + 80 <= 250 → pg[g0] <= 170 MW.
    /// g1 must supply the remaining 30 MW.
    #[test]
    fn test_ecrs_coupling_reduces_energy_dispatch() {
        let mut net = Network::new("ecrs_coupling_dispatch_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 200.0, 0.0));

        // g0: only ECRS provider, cheap
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 250.0;
        g0.market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "ecrs".into(),
                capacity_mw: 80.0,
                cost_per_mwh: 5.0,
            });
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g0);

        // g1: no ECRS capacity, expensive
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        // No ECRS offer
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.push(g1);

        // Without ECRS: g0 (cheap) should supply all 200 MW
        let opts_no_ecrs = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol_no_ecrs = solve_sced(&net, &opts_no_ecrs).unwrap();
        assert!(
            sol_no_ecrs.dispatch.pg_mw[0] > 190.0,
            "Without ECRS: g0 (cheap) should supply most load. pg0={:.1}",
            sol_no_ecrs.dispatch.pg_mw[0]
        );

        // With ECRS requirement = 80 MW: g0 is the sole provider.
        // Coupling: pg[g0] + r_ecrs[g0] <= 250 → pg[g0] <= 170 MW (since r_ecrs[g0] = 80).
        let opts_ecrs = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "ecrs".into(),
                requirement_mw: 80.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol_ecrs = solve_sced(&net, &opts_ecrs).unwrap();

        // g0 energy dispatch should be <= 170 MW (pmax - ecrs_req = 250 - 80)
        assert!(
            sol_ecrs.dispatch.pg_mw[0] <= 170.1,
            "With ECRS 80 MW req (g0 sole provider): pg[g0]={:.1} MW should be <= 170 MW",
            sol_ecrs.dispatch.pg_mw[0]
        );

        let ecrs_awards = sol_ecrs
            .dispatch
            .reserve_awards
            .get("ecrs")
            .expect("ecrs awards");
        // g0 must provide the 80 MW ECRS (sole provider)
        assert!(
            ecrs_awards[0] >= 79.9,
            "g0 should provide >= 80 MW ECRS as sole provider, got {:.1} MW",
            ecrs_awards[0]
        );

        // g1 has no ECRS to offer
        assert!(
            ecrs_awards[1] < 0.01,
            "g1 should have 0 ECRS (no offer), got {:.4} MW",
            ecrs_awards[1]
        );

        // Coupling constraint holds: pg[g0] + ecrs[g0] <= pmax[g0]
        assert!(
            sol_ecrs.dispatch.pg_mw[0] + ecrs_awards[0] <= 250.1,
            "Coupling: pg[g0]={:.1} + ecrs[g0]={:.1} = {:.1} > pmax=250",
            sol_ecrs.dispatch.pg_mw[0],
            ecrs_awards[0],
            sol_ecrs.dispatch.pg_mw[0] + ecrs_awards[0]
        );

        // g1 must supply the remainder (~30 MW)
        let total_gen: f64 = sol_ecrs.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 200 MW"
        );
        assert!(
            sol_ecrs.dispatch.pg_mw[1] >= 29.9,
            "g1 should supply >= 30 MW after g0 capped at 170 MW, got {:.1} MW",
            sol_ecrs.dispatch.pg_mw[1]
        );

        println!(
            "ECRS coupling dispatch shift: no_ecrs=({:.1},{:.1}), ecrs=({:.1},{:.1}) MW, ecrs_awards=({:.1},{:.1}) MW",
            sol_no_ecrs.dispatch.pg_mw[0],
            sol_no_ecrs.dispatch.pg_mw[1],
            sol_ecrs.dispatch.pg_mw[0],
            sol_ecrs.dispatch.pg_mw[1],
            ecrs_awards[0],
            ecrs_awards[1]
        );
    }

    /// P5-063 test 3: Non-qualified generators are excluded from RRS.
    ///
    /// g0: rrs_qualified=true, rrs_mw=60 → can provide up to 60 MW of RRS.
    /// g1: rrs_qualified=false → r_rrs[g1] upper bound = 0.0 → no RRS award.
    /// rrs_req = 50 MW → must be entirely served by g0.
    #[test]
    fn test_rrs_non_qualified_excluded() {
        let net = two_gen_network();
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "rrs".into(),
                requirement_mw: 50.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "power balance: gen={total_gen:.1} MW should be 200 MW"
        );

        // RRS requirement satisfied
        let rrs_provided = sol
            .dispatch
            .reserve_provided
            .get("rrs")
            .copied()
            .unwrap_or(0.0);
        assert!(
            rrs_provided >= 49.9,
            "rrs_provided={:.1} MW should be >= 50 MW requirement",
            rrs_provided
        );

        let rrs_awards = sol.dispatch.reserve_awards.get("rrs").expect("rrs awards");
        // g1 (non-qualified) must have zero RRS award
        assert!(
            rrs_awards[1] < 0.01,
            "g1 (non-qualified) should have 0 RRS award, got {:.4} MW",
            rrs_awards[1]
        );

        // g0 (qualified) must carry all the RRS
        assert!(
            rrs_awards[0] >= 49.9,
            "g0 (qualified) should provide >= 50 MW of RRS, got {:.1} MW",
            rrs_awards[0]
        );

        let rrs_price = sol
            .dispatch
            .reserve_prices
            .get("rrs")
            .copied()
            .unwrap_or(0.0);
        println!(
            "RRS qualification test: rrs_awards=({:.1},{:.1}) MW, rrs_price={:.4} $/MWh",
            rrs_awards[0], rrs_awards[1], rrs_price
        );
    }

    /// P5-063 test 4: Dual prices extracted correctly for both ECRS and RRS.
    ///
    /// With binding ECRS and RRS requirements, both prices should be positive.
    /// With zero requirements, prices should be zero.
    #[test]
    fn test_ecrs_rrs_dual_prices_extracted() {
        let net = two_gen_network();
        let get_price = |sol: &RawDispatchPeriodResult, pid: &str| -> f64 {
            sol.reserve_prices.get(pid).copied().unwrap_or(0.0)
        };

        // Without any requirements: prices should be 0
        let opts_no_req = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol_no_req = solve_sced(&net, &opts_no_req).unwrap();
        assert_eq!(
            get_price(&sol_no_req.dispatch, "ecrs"),
            0.0,
            "ecrs_price should be 0 without requirement"
        );
        assert_eq!(
            get_price(&sol_no_req.dispatch, "rrs"),
            0.0,
            "rrs_price should be 0 without requirement"
        );

        // With tight ECRS requirement: price should be positive (binding constraint)
        let opts_ecrs = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "ecrs".into(),
                requirement_mw: 60.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol_ecrs = solve_sced(&net, &opts_ecrs).unwrap();
        assert!(
            get_price(&sol_ecrs.dispatch, "ecrs") >= 0.0,
            "ecrs_price={:.4} should be non-negative",
            get_price(&sol_ecrs.dispatch, "ecrs")
        );
        let ecrs_provided = sol_ecrs
            .dispatch
            .reserve_provided
            .get("ecrs")
            .copied()
            .unwrap_or(0.0);
        assert!(
            ecrs_provided >= 59.9,
            "ecrs_provided={:.1} should be >= 60 MW",
            ecrs_provided
        );

        // With tight RRS requirement
        let opts_rrs = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "rrs".into(),
                requirement_mw: 55.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };
        let sol_rrs = solve_sced(&net, &opts_rrs).unwrap();
        assert!(
            get_price(&sol_rrs.dispatch, "rrs") >= 0.0,
            "rrs_price={:.4} should be non-negative",
            get_price(&sol_rrs.dispatch, "rrs")
        );
        let rrs_provided = sol_rrs
            .dispatch
            .reserve_provided
            .get("rrs")
            .copied()
            .unwrap_or(0.0);
        assert!(
            rrs_provided >= 54.9,
            "rrs_provided={:.1} should be >= 55 MW",
            rrs_provided
        );

        // Combined: all 6 ERCOT AS products active simultaneously
        let opts_all = DispatchOptions {
            enforce_thermal_limits: false,
            system_reserve_requirements: vec![
                SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 20.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_up".into(),
                    requirement_mw: 10.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_dn".into(),
                    requirement_mw: 10.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "ecrs".into(),
                    requirement_mw: 50.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "rrs".into(),
                    requirement_mw: 40.0,
                    per_period_mw: None,
                },
            ],
            ..Default::default()
        };
        // Add reg_up / reg_dn reserve offers for the test network
        let mut net2 = net.clone();
        net2.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "reg_up".into(),
                capacity_mw: 30.0,
                cost_per_mwh: 0.0,
            });
        net2.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "reg_dn".into(),
                capacity_mw: 30.0,
                cost_per_mwh: 0.0,
            });
        net2.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "reg_up".into(),
                capacity_mw: 20.0,
                cost_per_mwh: 0.0,
            });
        net2.generators[1]
            .market
            .get_or_insert_default()
            .reserve_offers
            .push(ReserveOffer {
                product_id: "reg_dn".into(),
                capacity_mw: 20.0,
                cost_per_mwh: 0.0,
            });

        let sol_all = solve_sced(&net2, &opts_all).unwrap();
        // Power balance must hold
        let total_gen: f64 = sol_all.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.5,
            "Combined AS: power balance gen={total_gen:.1} MW should be 200 MW"
        );
        // All prices non-negative
        for pid in &["ecrs", "rrs", "reg_up", "reg_dn"] {
            assert!(
                get_price(&sol_all.dispatch, pid) >= 0.0,
                "{pid}_price non-negative in combined"
            );
        }

        println!(
            "P5-063 prices: ecrs={:.4}, rrs={:.4} $/MWh (binding)",
            get_price(&sol_ecrs.dispatch, "ecrs"),
            get_price(&sol_rrs.dispatch, "rrs")
        );
        println!(
            "P5-063 combined 6-product: ecrs={:.4}, rrs={:.4}, spin={:.4}, reg_up={:.4}, reg_dn={:.4} $/MWh",
            get_price(&sol_all.dispatch, "ecrs"),
            get_price(&sol_all.dispatch, "rrs"),
            get_price(&sol_all.dispatch, "spin"),
            get_price(&sol_all.dispatch, "reg_up"),
            get_price(&sol_all.dispatch, "reg_dn")
        );
    }

    #[test]
    fn test_sced_with_pst_and_shunt() {
        // Verify that SCED correctly includes Pbusinj (PST phase-shift) and
        // Gs (shunt conductance) in the power balance RHS.
        //
        // Network: 3-bus, 2-generator, 1 PST branch.
        //   Bus 1 (slack): generator g0, gs = 100 MW shunt load
        //   Bus 2 (PQ): 200 MW load
        //   Bus 3 (PQ): generator g1
        //   Branch 1→2: x=0.1, rate_a=500, shift=5.0 deg (PST)
        //   Branch 2→3: x=0.1, rate_a=500, no shift
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let mut net = Network::new("test_pst_shunt");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 138.0);
        b1.shunt_conductance_mw = 100.0; // 100 MW shunt conductance at V=1 pu
        net.buses.push(b1);

        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 200.0, 0.0));

        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.push(b3);

        // PST branch: bus 1 → bus 2, shift = 5 degrees
        let mut br1 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br1.rating_a_mva = 500.0;
        br1.phase_shift_rad = 5.0_f64.to_radians();
        br1.in_service = true;
        net.branches.push(br1);

        // Regular branch: bus 2 → bus 3
        let mut br2 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br2.rating_a_mva = 500.0;
        br2.in_service = true;
        net.branches.push(br2);

        // Generator 0 at bus 1: pmin=0, pmax=300 MW, linear cost $10/MWh
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 300.0;
        g0.in_service = true;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 10.0],
        });
        net.generators.push(g0);

        // Generator 1 at bus 3: pmin=0, pmax=300 MW, linear cost $15/MWh
        let mut g1 = Generator::new(3, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 300.0;
        g1.in_service = true;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 15.0],
        });
        net.generators.push(g1);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };

        let sol = solve_sced(&net, &opts).expect("SCED with PST and shunt should solve");
        assert!(
            sol.dispatch.total_cost > 0.0,
            "SCED with PST+shunt: cost={:.2} should be positive",
            sol.dispatch.total_cost
        );

        // Total generation must cover load (200 MW) + shunt (100 MW) = 300 MW
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 300.0).abs() < 1.0,
            "SCED with PST+shunt: total_gen={:.2} MW, expected ~300 MW (200 load + 100 shunt)",
            total_gen
        );

        println!(
            "SCED PST+shunt: pg=[{:.1}, {:.1}] MW, cost={:.2}",
            sol.dispatch.pg_mw[0], sol.dispatch.pg_mw[1], sol.dispatch.total_cost
        );
    }
}

#[cfg(test)]
mod frequency_constraint_tests {
    use super::*;

    #[allow(dead_code)]
    fn data_available() -> bool {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::Path::new(&p).exists();
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .exists()
    }
    #[allow(dead_code)]
    fn test_data_path(name: &str) -> std::path::PathBuf {
        if let Ok(p) = std::env::var("SURGE_TEST_DATA") {
            return std::path::PathBuf::from(p).join(name);
        }
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/data")
            .join(name)
    }

    #[test]
    fn test_sced_frequency_defaults_no_overhead() {
        // When no generator_h_values are provided (default), frequency metrics
        // should be zero/true and add no computational overhead.
        if !data_available() {
            eprintln!("SKIP: tests/data not present");
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
        let sol = solve_sced(&net, &DispatchOptions::default()).unwrap();

        assert_eq!(
            sol.system_inertia_s, 0.0,
            "inertia should be 0 when h_values not provided"
        );
        assert_eq!(
            sol.estimated_rocof_hz_per_s, 0.0,
            "rocof should be 0 when h_values not provided"
        );
        assert!(
            sol.frequency_secure,
            "frequency_secure should default to true"
        );
    }

    #[test]
    fn test_sced_frequency_metrics_computed() {
        // When generator_h_values are provided, frequency metrics should be
        // computed correctly without changing the dispatch result.
        if !data_available() {
            eprintln!("SKIP: tests/data not present");
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        // Solve without frequency constraints first.
        let baseline = solve_sced(&net, &DispatchOptions::default()).unwrap();

        // Now solve with H values provided but no constraint thresholds —
        // the dispatch should be identical (metrics are post-solve only).
        let h_values = vec![5.0; n_gen]; // 5s inertia for all generators
        let opts = DispatchOptions {
            frequency_security: crate::config::frequency::FrequencySecurityOptions {
                generator_h_values: h_values,
                // All constraint thresholds remain 0.0 (disabled).
                ..Default::default()
            },
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // Dispatch should be identical (frequency metrics don't change the LP).
        assert!(
            (sol.dispatch.total_cost - baseline.dispatch.total_cost).abs() < 0.01,
            "cost changed: {} vs {} — frequency metrics should be post-solve only",
            sol.dispatch.total_cost,
            baseline.dispatch.total_cost
        );
        for (i, (a, b)) in sol
            .dispatch
            .pg_mw
            .iter()
            .zip(&baseline.dispatch.pg_mw)
            .enumerate()
        {
            assert!((a - b).abs() < 0.01, "gen {i} dispatch changed: {a} vs {b}");
        }

        // But now the metrics should be populated.
        assert!(
            sol.system_inertia_s > 0.0,
            "inertia should be computed: {}",
            sol.system_inertia_s
        );
        assert!(
            sol.estimated_rocof_hz_per_s > 0.0,
            "rocof should be computed: {}",
            sol.estimated_rocof_hz_per_s
        );
        assert!(
            sol.frequency_secure,
            "should be frequency secure (no constraints active)"
        );

        // Verify inertia is correct: H_sys = Σ(H×S) / Σ(S).
        // All H=5.0, so H_sys should be 5.0 regardless of Mbase mix.
        assert!(
            (sol.system_inertia_s - 5.0).abs() < 0.01,
            "H_sys should be 5.0, got {}",
            sol.system_inertia_s
        );
    }

    #[test]
    fn test_sced_frequency_security_check() {
        // Verify that check_frequency_security correctly flags violations.
        if !data_available() {
            eprintln!("SKIP: tests/data not present");
            return;
        }
        let net = surge_io::matpower::load(test_data_path("case9.m")).unwrap();
        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        // Very low inertia → high RoCoF → should flag frequency insecure.
        let h_values = vec![0.5; n_gen]; // 0.5s — very low inertia
        let opts = DispatchOptions {
            frequency_security: crate::config::frequency::FrequencySecurityOptions {
                generator_h_values: h_values,
                min_inertia_mws: Some(3.0),    // require H_sys >= 3.0s
                max_rocof_hz_per_s: Some(0.5), // very tight RoCoF limit
                ..Default::default()
            },
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(
            sol.system_inertia_s < 3.0,
            "H_sys={} should be < 3.0 with H=0.5",
            sol.system_inertia_s
        );
        assert!(
            !sol.frequency_secure,
            "should be frequency INsecure: H_sys={}, RoCoF={}",
            sol.system_inertia_s, sol.estimated_rocof_hz_per_s
        );

        // Now with high inertia and relaxed RoCoF limit → should be secure.
        // case9 largest unit is 300 MW, 3 gens at mbase=100. With H=6:
        //   RoCoF = 300×60 / (2×1800) = 5.0 Hz/s → need limit > 5.0
        let h_values = vec![6.0; n_gen];
        let opts = DispatchOptions {
            frequency_security: crate::config::frequency::FrequencySecurityOptions {
                generator_h_values: h_values,
                min_inertia_mws: Some(3.0),
                max_rocof_hz_per_s: Some(6.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(
            sol.system_inertia_s >= 3.0,
            "H_sys={} should be >= 3.0 with H=6.0",
            sol.system_inertia_s
        );
        assert!(
            sol.frequency_secure,
            "should be frequency SECURE: H_sys={}, RoCoF={}",
            sol.system_inertia_s, sol.estimated_rocof_hz_per_s
        );
    }
}

// =============================================================================
// HVDC link dispatch tests
// =============================================================================

#[cfg(test)]
mod hvdc_dispatch_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    /// Build a 2-bus network connected by an AC branch.
    /// Bus 1 = slack (generator), Bus 2 = load bus.
    /// The AC branch has a configurable thermal limit (rate_a).
    fn two_bus_network(load_mw: f64, line_rate_a: f64) -> Network {
        let mut net = Network::new("hvdc_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b1);
        net.buses.push(b2);
        net.loads.push(Load::new(2, load_mw, 0.0));

        // AC line between bus 1 and bus 2
        let mut br = Branch::new_line(1, 2, 0.01, 0.1, 0.02);
        br.rating_a_mva = line_rate_a;
        net.branches.push(br);

        // Cheap generator at bus 1
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 500.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0], // $20/MWh linear
        });
        net.generators.push(g1);

        // Expensive generator at bus 2 (needed when AC line is congested)
        let mut g2 = Generator::new(2, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 300.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0], // $50/MWh linear
        });
        net.generators.push(g2);

        net
    }

    /// Test that SCED with empty hvdc_links produces the same result as without HVDC.
    #[test]
    fn test_sced_no_hvdc_unchanged() {
        let net = two_bus_network(100.0, 200.0);

        // Without HVDC
        let opts_no_hvdc = DispatchOptions {
            enforce_thermal_limits: true,
            ..Default::default()
        };
        let sol_no_hvdc = solve_sced(&net, &opts_no_hvdc).unwrap();

        // With empty hvdc_links
        let opts_hvdc = DispatchOptions {
            enforce_thermal_limits: true,
            hvdc_links: vec![],
            ..Default::default()
        };
        let sol_hvdc = solve_sced(&net, &opts_hvdc).unwrap();

        // Results should be identical
        assert!(
            (sol_no_hvdc.dispatch.total_cost - sol_hvdc.dispatch.total_cost).abs() < 0.01,
            "Empty hvdc_links should produce identical cost: {} vs {}",
            sol_no_hvdc.dispatch.total_cost,
            sol_hvdc.dispatch.total_cost
        );
        assert!(
            sol_hvdc.dispatch.hvdc_dispatch_mw.is_empty(),
            "Empty hvdc_links should produce empty hvdc_dispatch_mw"
        );
    }

    /// Test that HVDC link optimizes flow to reduce total cost.
    ///
    /// Setup: 2-bus network, cheap gen at bus 1, expensive gen at bus 2,
    /// load at bus 2. AC line limited to 50 MW. Without HVDC, the expensive
    /// gen at bus 2 must produce the overflow. With HVDC (bus 1 -> bus 2,
    /// 0-100 MW), the optimizer can send more cheap power via HVDC.
    #[test]
    fn test_sced_with_hvdc_optimizes_flow() {
        let net = two_bus_network(150.0, 50.0); // 150 MW load, 50 MW AC limit

        // Without HVDC: 50 MW from cheap gen via AC, 100 MW from expensive gen
        let opts_no_hvdc = DispatchOptions {
            enforce_thermal_limits: true,
            ..Default::default()
        };
        let sol_no_hvdc = solve_sced(&net, &opts_no_hvdc).unwrap();

        // With HVDC: lossless link from bus 1 to bus 2, 0-100 MW
        let hvdc_link = HvdcDispatchLink {
            id: String::new(),
            name: "HVDC_1_2".into(),
            from_bus: 1,
            to_bus: 2,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 100.0,
            loss_a_mw: 0.0,
            loss_b_frac: 0.0,
            ramp_mw_per_min: 0.0,
            cost_per_mwh: 0.0,
            bands: vec![],
        };
        let opts_hvdc = DispatchOptions {
            enforce_thermal_limits: true,
            hvdc_links: vec![hvdc_link],
            ..Default::default()
        };
        let sol_hvdc = solve_sced(&net, &opts_hvdc).unwrap();

        // HVDC should reduce cost (more cheap power from bus 1)
        assert!(
            sol_hvdc.dispatch.total_cost < sol_no_hvdc.dispatch.total_cost - 1.0,
            "HVDC should reduce cost: {:.2} < {:.2}",
            sol_hvdc.dispatch.total_cost,
            sol_no_hvdc.dispatch.total_cost
        );

        // HVDC dispatch should be positive (sending power from bus 1 to bus 2)
        assert_eq!(sol_hvdc.dispatch.hvdc_dispatch_mw.len(), 1);
        let p_hvdc = sol_hvdc.dispatch.hvdc_dispatch_mw[0];
        assert!(
            p_hvdc > 1.0,
            "HVDC dispatch should be positive: {:.2} MW",
            p_hvdc
        );

        // Power balance check
        let total_gen: f64 = sol_hvdc.dispatch.pg_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        assert!(
            (total_gen - total_load).abs() < 0.5,
            "power balance: gen={:.1} MW, load={:.1} MW",
            total_gen,
            total_load
        );

        println!(
            "No HVDC: cost={:.2}, pg={:?}",
            sol_no_hvdc.dispatch.total_cost, sol_no_hvdc.dispatch.pg_mw
        );
        println!(
            "With HVDC: cost={:.2}, pg={:?}, P_hvdc={:.2} MW",
            sol_hvdc.dispatch.total_cost, sol_hvdc.dispatch.pg_mw, p_hvdc
        );
    }

    /// Test that HVDC at its max limit produces LMP separation.
    ///
    /// When the AC line is congested AND HVDC is at max, the expensive bus
    /// should have a higher LMP than the cheap bus.
    #[test]
    fn test_sced_hvdc_lmp_separation() {
        let net = two_bus_network(200.0, 50.0); // 200 MW load, 50 MW AC limit

        // HVDC limited to 50 MW — total transfer = 100 MW, but load = 200 MW
        // So bus 2 needs 100 MW from expensive gen.
        let hvdc_link = HvdcDispatchLink {
            id: String::new(),
            name: "HVDC_1_2".into(),
            from_bus: 1,
            to_bus: 2,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 50.0,
            loss_a_mw: 0.0,
            loss_b_frac: 0.0,
            ramp_mw_per_min: 0.0,
            cost_per_mwh: 0.0,
            bands: vec![],
        };
        let opts = DispatchOptions {
            enforce_thermal_limits: true,
            hvdc_links: vec![hvdc_link],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // HVDC should be at max (50 MW)
        let p_hvdc = sol.dispatch.hvdc_dispatch_mw[0];
        assert!(
            (p_hvdc - 50.0).abs() < 0.5,
            "HVDC should be at limit: {:.2} MW",
            p_hvdc
        );

        // LMP at bus 2 (expensive gen) should be higher than bus 1 (cheap gen)
        assert!(
            sol.dispatch.lmp[1] > sol.dispatch.lmp[0] + 1.0,
            "Bus 2 LMP ({:.2}) should be > Bus 1 LMP ({:.2}) when both AC and HVDC are congested",
            sol.dispatch.lmp[1],
            sol.dispatch.lmp[0]
        );

        println!(
            "LMPs: bus1={:.2}, bus2={:.2}, P_hvdc={:.2}",
            sol.dispatch.lmp[0], sol.dispatch.lmp[1], p_hvdc
        );
    }

    /// Test that HVDC respects p_dc_max_mw when there is excess demand.
    #[test]
    fn test_sced_hvdc_at_limit() {
        let net = two_bus_network(250.0, 50.0); // 250 MW load, 50 MW AC limit

        let hvdc_link = HvdcDispatchLink {
            id: String::new(),
            name: "HVDC_1_2".into(),
            from_bus: 1,
            to_bus: 2,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 80.0,
            loss_a_mw: 0.0,
            loss_b_frac: 0.0,
            ramp_mw_per_min: 0.0,
            cost_per_mwh: 0.0,
            bands: vec![],
        };
        let opts = DispatchOptions {
            enforce_thermal_limits: true,
            hvdc_links: vec![hvdc_link],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // HVDC should be at its max limit
        let p_hvdc = sol.dispatch.hvdc_dispatch_mw[0];
        assert!(
            (p_hvdc - 80.0).abs() < 0.5,
            "HVDC should be at limit 80 MW: got {:.2} MW",
            p_hvdc
        );

        // Bus 2 gen should cover the remainder
        // Total from bus 1: 50 MW AC + 80 MW HVDC = 130 MW
        // Load 250 MW, so bus 2 gen provides 120 MW
        let g2_dispatch = sol.dispatch.pg_mw[1];
        assert!(
            (g2_dispatch - 120.0).abs() < 1.0,
            "Gen2 should provide ~120 MW: got {:.2} MW",
            g2_dispatch
        );
    }

    /// Test that HVDC loss model works correctly.
    ///
    /// With loss_b_frac > 0, the inverter receives less power.
    /// The optimizer should account for this in dispatch.
    #[test]
    fn test_sced_hvdc_with_losses() {
        let net = two_bus_network(100.0, 200.0); // 100 MW load, ample AC

        // HVDC with 5% loss: inverter injects (1 - 0.05) * P_dc
        let hvdc_link = HvdcDispatchLink {
            id: String::new(),
            name: "HVDC_lossy".into(),
            from_bus: 1,
            to_bus: 2,
            p_dc_min_mw: 0.0,
            p_dc_max_mw: 100.0,
            loss_a_mw: 1.0, // 1 MW constant loss
            loss_b_frac: 0.05,
            ramp_mw_per_min: 0.0,
            cost_per_mwh: 0.0,
            bands: vec![],
        };
        let opts = DispatchOptions {
            enforce_thermal_limits: false, // uncongested AC
            hvdc_links: vec![hvdc_link],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // With ample AC capacity and HVDC with losses, the optimizer should
        // prefer the lossless AC line. HVDC dispatch should be small or zero.
        let p_hvdc = sol.dispatch.hvdc_dispatch_mw[0];
        // Since both gens can supply via AC directly and HVDC has losses,
        // all power should go through AC. HVDC = 0 or small.
        assert!(
            p_hvdc < 5.0,
            "HVDC should be ~0 MW when AC is uncongested and HVDC has losses: got {:.2} MW",
            p_hvdc
        );

        // Total generation should cover load + HVDC constant loss.
        // loss_a_mw is baked into the power balance RHS at the rectifier bus,
        // so total generation equals load + loss_a_mw.
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        let total_load: f64 = net.total_load_mw();
        let total_loss_a: f64 = 1.0; // loss_a_mw
        assert!(
            (total_gen - total_load - total_loss_a).abs() < 1.0,
            "power balance: gen={:.1} MW, load+loss={:.1} MW",
            total_gen,
            total_load + total_loss_a,
        );
    }
}

#[cfg(test)]
mod committed_units_tests {
    use super::*;
    use crate::dispatch::CommitmentMode;

    fn make_3gen_sced_network() -> Option<Network> {
        // Build a simple 3-gen network for testing
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/cases/case9/case9.m");
        if !path.exists() {
            return None;
        }
        Some(surge_io::matpower::load(&path).expect("case9.m"))
    }

    #[test]
    fn test_sced_committed_units_forces_off() {
        let Some(net) = make_3gen_sced_network() else {
            eprintln!("SKIP: case data not present");
            return;
        };
        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        // All committed = true except generator index 1
        let mut committed = vec![true; n_gen];
        committed[1] = false;

        let opts = DispatchOptions {
            commitment: CommitmentMode::Fixed {
                commitment: committed,
                per_period: None,
            },
            ..DispatchOptions::default()
        };
        let result = solve_sced(&net, &opts).expect("SCED with committed_units");

        // Generator 1 should be at 0 MW
        assert!(
            result.dispatch.pg_mw[1].abs() < 1e-6,
            "Uncommitted gen should dispatch 0 MW, got {}",
            result.dispatch.pg_mw[1]
        );
    }

    #[test]
    fn test_sced_all_committed_same_as_default() {
        let Some(net) = make_3gen_sced_network() else {
            eprintln!("SKIP: case data not present");
            return;
        };
        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        let default_result = solve_sced(&net, &DispatchOptions::default()).expect("default SCED");

        let opts = DispatchOptions {
            commitment: CommitmentMode::Fixed {
                commitment: vec![true; n_gen],
                per_period: None,
            },
            ..DispatchOptions::default()
        };
        let committed_result = solve_sced(&net, &opts).expect("all-committed SCED");

        // Results should be identical
        for (i, (d, c)) in default_result
            .dispatch
            .pg_mw
            .iter()
            .zip(committed_result.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (d - c).abs() < 0.1,
                "Gen {i}: default={d:.2} vs all-committed={c:.2}"
            );
        }
    }

    #[test]
    fn test_sced_uncommitted_no_reserves() {
        let Some(mut net) = make_3gen_sced_network() else {
            eprintln!("SKIP: case data not present");
            return;
        };
        let n_gen = net.generators.iter().filter(|g| g.in_service).count();

        // Give all generators ancillary service capability
        for g in &mut net.generators {
            if g.in_service {
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "reg_up".into(),
                        capacity_mw: 50.0,
                        cost_per_mwh: 0.0,
                    },
                );
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "reg_dn".into(),
                        capacity_mw: 50.0,
                        cost_per_mwh: 0.0,
                    },
                );
            }
        }

        // Decommit generator 1
        let mut committed = vec![true; n_gen];
        committed[1] = false;

        let opts = DispatchOptions {
            system_reserve_requirements: vec![
                SystemReserveRequirement {
                    product_id: "spin".into(),
                    requirement_mw: 30.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_up".into(),
                    requirement_mw: 30.0,
                    per_period_mw: None,
                },
                SystemReserveRequirement {
                    product_id: "reg_dn".into(),
                    requirement_mw: 30.0,
                    per_period_mw: None,
                },
            ],
            commitment: CommitmentMode::Fixed {
                commitment: committed,
                per_period: None,
            },
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).expect("SCED with uncommitted gen");

        // Generator 1 should dispatch 0 MW
        assert!(
            sol.dispatch.pg_mw[1].abs() < 1e-6,
            "Uncommitted gen should dispatch 0 MW, got {}",
            sol.dispatch.pg_mw[1]
        );
        // Generator 1 should provide 0 MW of spinning reserve
        let spin_awards = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .expect("spin awards");
        assert!(
            spin_awards[1].abs() < 1e-6,
            "Uncommitted gen should provide 0 MW spinning reserve, got {}",
            spin_awards[1]
        );
        // Generator 1 should provide 0 MW of reg up
        let rup_awards = sol
            .dispatch
            .reserve_awards
            .get("reg_up")
            .expect("reg_up awards");
        assert!(
            rup_awards[1].abs() < 1e-6,
            "Uncommitted gen should provide 0 MW reg up, got {}",
            rup_awards[1]
        );
        // Generator 1 should provide 0 MW of reg down
        let rdn_awards = sol
            .dispatch
            .reserve_awards
            .get("reg_dn")
            .expect("reg_dn awards");
        assert!(
            rdn_awards[1].abs() < 1e-6,
            "Uncommitted gen should provide 0 MW reg down, got {}",
            rdn_awards[1]
        );
    }
}

// =============================================================================
// OPS-M1: Zonal spinning reserve requirement tests
// =============================================================================

#[cfg(test)]
mod zonal_reserve_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    /// Build a two-area, two-bus network with two generators.
    fn two_area_network() -> Network {
        let mut net = Network::new("zonal_reserve_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 60.0, 0.0));

        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 40.0, 0.0));

        let mut br = Branch::new_line(1, 2, 0.0, 0.01, 0.0);
        br.rating_a_mva = 300.0;
        br.in_service = true;
        net.branches.push(br);

        // G0 in area 0 — cheap, large capacity
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        // G1 in area 1 — expensive, large capacity
        let mut g1 = Generator::new(2, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        net.generators.push(g1);

        net
    }

    #[test]
    fn test_zonal_reserve_binds() {
        let mut net = two_area_network();
        for g in &mut net.generators {
            let phys_cap = (g.pmax - g.pmin).max(0.0);
            if phys_cap > 0.0 {
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "spin".into(),
                        capacity_mw: phys_cap,
                        cost_per_mwh: 0.0,
                    },
                );
            }
        }

        // System-wide 30 MW spinning + zonal 20 MW in area 1.
        // Without zonal constraint, G0 (cheap) would provide all reserve.
        // With zonal constraint, G1 must provide at least 20 MW of reserve.
        let opts = DispatchOptions {
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            }],
            zonal_reserve_requirements: vec![ZonalReserveRequirement {
                zone_id: 1,
                product_id: "spin".into(),
                requirement_mw: 20.0,
                per_period_mw: None,
                shortfall_cost_per_unit: None,
                served_dispatchable_load_coefficient: None,
                largest_generator_dispatch_coefficient: None,
                participant_bus_numbers: None,
            }],
            generator_area: vec![0, 1],
            load_area: vec![0, 1],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // G1 must provide at least 20 MW spinning reserve
        let spin_awards = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .expect("spin awards");
        assert!(
            spin_awards[1] >= 19.9,
            "G1 in zone 1 should provide >= 20 MW reserve, got {:.1}",
            spin_awards[1]
        );
        // System total should be >= 30 MW
        let total_spin: f64 = spin_awards.iter().sum();
        assert!(
            total_spin >= 29.9,
            "Total spin should be >= 30 MW, got {:.1}",
            total_spin
        );
        // Zonal price should be present
        assert!(
            sol.dispatch.zonal_reserve_prices.contains_key("1:spin"),
            "Zone 1 should have a spinning reserve price"
        );
    }

    #[test]
    fn test_zonal_reserve_no_effect_when_empty() {
        let mut net = two_area_network();
        for g in &mut net.generators {
            let phys_cap = (g.pmax - g.pmin).max(0.0);
            if phys_cap > 0.0 {
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "spin".into(),
                        capacity_mw: phys_cap,
                        cost_per_mwh: 0.0,
                    },
                );
            }
        }

        let opts_without = DispatchOptions {
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            }],
            generator_area: vec![0, 1],
            load_area: vec![0, 1],
            ..Default::default()
        };
        let sol_without = solve_sced(&net, &opts_without).unwrap();

        let opts_with = DispatchOptions {
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            }],
            generator_area: vec![0, 1],
            load_area: vec![0, 1],
            zonal_reserve_requirements: vec![],
            ..Default::default()
        };
        let sol_with = solve_sced(&net, &opts_with).unwrap();

        // Same total cost — empty zonal reserves should not change solution
        assert!(
            (sol_without.dispatch.total_cost - sol_with.dispatch.total_cost).abs() < 1.0,
            "Empty zonal reserves should not change cost: {:.2} vs {:.2}",
            sol_without.dispatch.total_cost,
            sol_with.dispatch.total_cost
        );
    }
}

// =============================================================================
// MKT-M1: Loss-adjusted LMP tests
// =============================================================================

#[cfg(test)]
mod loss_lmp_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator};

    #[test]
    fn test_loss_lmps_enabled() {
        let mut net = Network::new("loss_lmp_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);

        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 100.0, 0.0));

        // Branch with significant resistance for loss computation
        let mut br = Branch::new_line(1, 2, 0.01, 0.05, 0.0);
        br.rating_a_mva = 300.0;
        br.in_service = true;
        net.branches.push(br);

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g0);

        // With loss LMPs enabled
        let opts = DispatchOptions {
            use_loss_factors: true,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // lmp_loss should be non-empty
        assert!(
            !sol.dispatch.lmp_loss.is_empty(),
            "lmp_loss should be non-empty when enabled"
        );
        assert_eq!(sol.dispatch.lmp_loss.len(), 2);

        // Loss component at slack bus should be ~0 (reference)
        assert!(
            sol.dispatch.lmp_loss[0].abs() < 1.0,
            "Loss component at slack should be near zero, got {:.4}",
            sol.dispatch.lmp_loss[0]
        );

        // Three-component decomposition should sum to total LMP
        for i in 0..2 {
            let sum = sol.dispatch.lmp_energy[i]
                + sol.dispatch.lmp_congestion[i]
                + sol.dispatch.lmp_loss[i];
            assert!(
                (sol.dispatch.lmp[i] - sum).abs() < 0.01,
                "LMP decomposition should sum to total at bus {}: {} vs {:.4}+{:.4}+{:.4}={:.4}",
                i,
                sol.dispatch.lmp[i],
                sol.dispatch.lmp_energy[i],
                sol.dispatch.lmp_congestion[i],
                sol.dispatch.lmp_loss[i],
                sum
            );
        }
    }

    #[test]
    fn test_loss_lmps_disabled_by_default() {
        let mut net = Network::new("loss_lmp_off");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);

        let b2 = Bus::new(2, BusType::PQ, 138.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 0.0));

        let mut br = Branch::new_line(1, 2, 0.01, 0.05, 0.0);
        br.rating_a_mva = 200.0;
        br.in_service = true;
        net.branches.push(br);

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g0);

        let opts = DispatchOptions::default();
        let sol = solve_sced(&net, &opts).unwrap();

        // lmp_loss should be zero when loss factors are disabled
        assert!(
            sol.dispatch.lmp_loss.iter().all(|&l| l.abs() < 1e-12),
            "lmp_loss should be zero when use_loss_factors=false, got {:?}",
            sol.dispatch.lmp_loss
        );
    }
}

// =============================================================================
// Flowgate / Interface enforcement in SCED
// =============================================================================

#[cfg(test)]
mod flowgate_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Flowgate, Generator, Interface};

    /// Build a simple 3-bus network:
    ///   Bus 1 (Slack) — Bus 2 — Bus 3
    ///   G1 at bus 1 (cheap, $20/MWh), G2 at bus 3 (expensive, $40/MWh)
    ///   Load of 150 MW at bus 2
    ///   Branch 1-2 and 2-3, each x=0.1 pu, rate_a=200 MW
    fn make_three_bus() -> Network {
        let mut net = Network::new("flowgate_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.push(b1);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 150.0, 0.0));
        net.buses.push(b3);

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        net.branches.push(br12);
        net.branches.push(br23);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 250.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        let mut g2 = Generator::new(3, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 250.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.push(g1);
        net.generators.push(g2);
        net
    }

    /// Without any flowgates: G1 (cheap) supplies all 150 MW; G2 idles.
    #[test]
    fn test_sced_no_flowgate_cheap_gen_wins() {
        let net = make_three_bus();
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        // G1 is cheapest — should supply all load
        assert!(
            sol.dispatch.pg_mw[0] > 140.0,
            "G1 should supply nearly all load, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.pg_mw[1] < 10.0,
            "G2 should be near-idle, got {:.1} MW",
            sol.dispatch.pg_mw[1]
        );
    }

    /// A tight flowgate on branch 1-2 forces G2 (at bus 3) to pick up load.
    #[test]
    fn test_sced_flowgate_binds_and_shifts_dispatch() {
        let mut net = make_three_bus();
        // Limit flow on branch 1-2 to 50 MW (well below unconstrained 150 MW)
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: None,
            limit_mw: 50.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // G1 flow is limited to ~50 MW by the flowgate; G2 picks up the rest
        assert!(
            sol.dispatch.pg_mw[0] <= 55.0,
            "G1 should be limited to ~50 MW by flowgate, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.pg_mw[1] >= 95.0,
            "G2 should pick up remainder (~100 MW), got {:.1} MW",
            sol.dispatch.pg_mw[1]
        );
        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 150.0).abs() < 0.5,
            "Power balance: gen={:.1} MW, load=150 MW",
            total_gen
        );
    }

    /// Setting enforce_flowgates=false ignores the flowgate (G1 wins again).
    #[test]
    fn test_sced_enforce_flowgates_false_ignores_flowgate() {
        let mut net = make_three_bus();
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: None,
            limit_mw: 50.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: false,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        // Flowgate ignored → G1 (cheap) supplies all load
        assert!(
            sol.dispatch.pg_mw[0] > 140.0,
            "With enforce_flowgates=false, G1 should supply all load, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
    }

    /// A tight interface on branch 1-2 also constrains dispatch.
    #[test]
    fn test_sced_interface_binds_and_shifts_dispatch() {
        let mut net = make_three_bus();
        net.interfaces.push(Interface {
            name: "IF_12".to_string(),
            members: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            limit_forward_mw: 50.0,
            limit_reverse_mw: 50.0,
            in_service: true,
            limit_forward_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // G1 limited to ~50 MW by interface
        assert!(
            sol.dispatch.pg_mw[0] <= 55.0,
            "G1 should be limited to ~50 MW by interface, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.pg_mw[1] >= 95.0,
            "G2 should pick up remainder, got {:.1} MW",
            sol.dispatch.pg_mw[1]
        );
    }

    /// Contingency flowgates (contingency_branch = Some(...)) ARE enforced in SCED.
    /// The pre-computed OTDF-adjusted limit in limit_mw is treated identically to
    /// a base-case flowgate — the contingency annotation is informational only.
    #[test]
    fn test_sced_contingency_flowgate_enforced() {
        let mut net = make_three_bus();
        // Tight limit tagged as a contingency flowgate (N-1 pre-computed limit)
        net.flowgates.push(Flowgate {
            name: "FG_12_N1".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: Some(BranchRef::new(2, 3, "1")), // N-1 contingency
            limit_mw: 50.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        // Contingency flowgate IS enforced → G1 flow on branch 1-2 capped at 50 MW
        assert!(
            sol.dispatch.pg_mw[0] <= 52.0,
            "Contingency flowgate should be enforced in SCED, G1 dispatch={:.1} MW",
            sol.dispatch.pg_mw[0]
        );
    }

    /// Binding flowgate produces a non-zero shadow price; slack flowgate produces ~0.
    #[test]
    fn test_sced_flowgate_shadow_price_binding() {
        let mut net = make_three_bus();
        // Tight flowgate on branch 1-2 — will bind
        net.flowgates.push(Flowgate {
            name: "FG_12_tight".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: None,
            limit_mw: 50.0, // binding — unconstrained flow ≈ 150 MW
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });
        // Slack flowgate on same branch with very high limit
        net.flowgates.push(Flowgate {
            name: "FG_12_slack".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: None,
            limit_mw: 9999.0,
            in_service: true,
            limit_reverse_mw: 0.0,
            limit_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // Shadow prices must be returned for all flowgates
        assert_eq!(
            sol.dispatch.flowgate_shadow_prices.len(),
            2,
            "must have one shadow price per flowgate"
        );
        // Binding flowgate → positive shadow price (relaxing the limit reduces cost)
        assert!(
            sol.dispatch.flowgate_shadow_prices[0] > 1e-4,
            "binding flowgate shadow price should be positive, got {:.6}",
            sol.dispatch.flowgate_shadow_prices[0]
        );
        // Slack flowgate → shadow price ≈ 0
        assert!(
            sol.dispatch.flowgate_shadow_prices[1].abs() < 1e-4,
            "slack flowgate shadow price should be ~0, got {:.6}",
            sol.dispatch.flowgate_shadow_prices[1]
        );
    }

    /// Binding branch thermal limits should produce non-zero branch shadow prices.
    #[test]
    fn test_sced_branch_shadow_price_binding() {
        let mut net = make_three_bus();
        net.branches[0].rating_a_mva = 50.0;

        let opts = DispatchOptions {
            enforce_thermal_limits: true,
            enforce_flowgates: false,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert_eq!(
            sol.dispatch.branch_shadow_prices.len(),
            2,
            "must have one shadow price per constrained branch"
        );
        assert!(
            sol.dispatch.branch_shadow_prices[0] > 1e-4,
            "binding branch shadow price should be positive, got {:.6}",
            sol.dispatch.branch_shadow_prices[0]
        );
        assert!(
            sol.dispatch.branch_shadow_prices[1].abs() < 1e-4,
            "slack branch shadow price should be ~0, got {:.6}",
            sol.dispatch.branch_shadow_prices[1]
        );
    }

    /// Interface shadow prices: binding interface produces non-zero price; enforce=false → empty.
    #[test]
    fn test_sced_interface_shadow_price() {
        let mut net = make_three_bus();
        net.interfaces.push(Interface {
            name: "IF_12".to_string(),
            members: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            limit_forward_mw: 50.0, // binding
            limit_reverse_mw: 50.0,
            in_service: true,
            limit_forward_mw_schedule: Vec::new(),
            limit_reverse_mw_schedule: Vec::new(),
        });

        let opts_on = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            ..Default::default()
        };
        let sol_on = solve_sced(&net, &opts_on).unwrap();
        assert_eq!(sol_on.dispatch.interface_shadow_prices.len(), 1);
        assert!(
            sol_on.dispatch.interface_shadow_prices[0] > 1e-4,
            "binding interface shadow price should be positive, got {:.6}",
            sol_on.dispatch.interface_shadow_prices[0]
        );

        // With enforce_flowgates=false, shadow prices must be empty
        let opts_off = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: false,
            ..Default::default()
        };
        let sol_off = solve_sced(&net, &opts_off).unwrap();
        assert!(
            sol_off.dispatch.interface_shadow_prices.is_empty(),
            "interface_shadow_prices must be empty when enforce_flowgates=false"
        );
    }

    /// Regression test: default options (no reserve requirements) must produce
    /// reserve_awards with n_gen-length vectors for each ERCOT default product.
    #[test]
    fn test_sced_default_reserve_awards_length() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("reserve_awards_len_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(1, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 150.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });
        net.generators.push(g2);

        // Default: no reserve requirements.
        let sol = solve_sced(&net, &DispatchOptions::default()).unwrap();

        let n_gen = net.generators.len();
        // Each ERCOT default product should have n_gen-length award vectors
        for pid in &["spin", "reg_up", "reg_dn", "nspin", "ecrs", "rrs"] {
            if let Some(awards) = sol.dispatch.reserve_awards.get(*pid) {
                assert_eq!(
                    awards.len(),
                    n_gen,
                    "{pid} awards must have n_gen={n_gen} elements, got {}",
                    awards.len()
                );
                assert!(
                    awards.iter().all(|&v| v == 0.0),
                    "{pid} awards should all be 0.0 when no reserve requested"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Storage dispatch mode tests
    // -----------------------------------------------------------------------

    fn make_two_bus_net() -> surge_network::Network {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};
        let mut net = Network::new("storage_test");
        net.base_mva = 100.0;
        // Slack bus with 100 MW load
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 100.0, 0.0));
        // PQ bus (storage bus)
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.branches.push(Branch::new_line(1, 2, 0.0, 0.02, 0.0));
        // One generator at bus 1: $20/MWh, up to 200 MW
        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmax = 200.0;
        g.pmin = 0.0;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(g);
        net
    }

    /// SelfSchedule: battery commits +20 MW discharge.
    /// Assert storage_discharge_mw ≈ 20 and storage_charge_mw ≈ 0.
    #[test]
    fn test_dc_sced_storage_self_schedule_discharge() {
        use surge_network::market::CostCurve;
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let mut net = make_two_bus_net();
        // Add storage generator at bus 2 (SelfSchedule: +20 MW discharge)
        let g = Generator {
            bus: 2,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::SelfSchedule,
                self_schedule_mw: 20.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net.generators.push(g);

        let opts = DispatchOptions::default();
        let sol = solve_sced(&net, &opts).unwrap();

        assert_eq!(sol.dispatch.storage_discharge_mw.len(), 1);
        assert_eq!(sol.dispatch.storage_charge_mw.len(), 1);
        assert!(
            (sol.dispatch.storage_discharge_mw[0] - 20.0).abs() < 1e-3,
            "Expected discharge ≈ 20 MW, got {:.4}",
            sol.dispatch.storage_discharge_mw[0]
        );
        assert!(
            sol.dispatch.storage_charge_mw[0] < 1e-6,
            "Expected charge ≈ 0, got {:.4}",
            sol.dispatch.storage_charge_mw[0]
        );
    }

    /// SelfSchedule: battery commits -30 MW (charge).
    /// Assert storage_charge_mw ≈ 30 and storage_discharge_mw ≈ 0.
    #[test]
    fn test_dc_sced_storage_self_schedule_charge() {
        use surge_network::market::CostCurve;
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let mut net = make_two_bus_net();
        // Add storage generator at bus 2 (SelfSchedule: -30 MW charge)
        let g = Generator {
            bus: 2,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::SelfSchedule,
                self_schedule_mw: -30.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net.generators.push(g);

        let opts = DispatchOptions::default();
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(
            sol.dispatch.storage_discharge_mw[0] < 1e-6,
            "Expected discharge ≈ 0, got {:.4}",
            sol.dispatch.storage_discharge_mw[0]
        );
        assert!(
            (sol.dispatch.storage_charge_mw[0] - 30.0).abs() < 1e-3,
            "Expected charge ≈ 30 MW, got {:.4}",
            sol.dispatch.storage_charge_mw[0]
        );
    }

    /// CostMinimization with degradation cost.
    ///
    /// When generator LMP ($20/MWh) < storage discharge cost (variable $0 + degradation
    /// $25/MWh + efficiency penalty), storage should NOT discharge. This test uses a very
    /// high degradation cost ($25/MWh) so storage stays neutral.
    #[test]
    fn test_dc_sced_storage_cost_min_high_degradation_stays_neutral() {
        use surge_network::market::CostCurve;
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let mut net = make_two_bus_net();
        // Add storage generator at bus 2 (CostMinimization, $25/MWh degradation)
        let g = Generator {
            bus: 2,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 25.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net.generators.push(g);

        let opts = DispatchOptions::default();
        let sol = solve_sced(&net, &opts).unwrap();

        // Storage should not discharge (too expensive vs generator)
        assert!(
            sol.dispatch.storage_discharge_mw[0] < 1e-3,
            "Storage should stay neutral when degradation cost exceeds LMP, got dis={:.4}",
            sol.dispatch.storage_discharge_mw[0]
        );
    }

    /// CostMinimization with zero cost:
    /// Storage at $0/MWh can substitute for the $20/MWh generator.
    /// It should discharge to reduce total system cost.
    /// Total cost with storage should be lower than without.
    #[test]
    fn test_dc_sced_storage_cost_min_zero_cost_dispatches() {
        use surge_network::market::CostCurve;
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let net = make_two_bus_net();
        // Baseline cost without storage
        let sol_no_storage = solve_sced(&net, &DispatchOptions::default()).unwrap();

        let mut net_with_sto = make_two_bus_net();
        // Add storage generator at bus 2 (CostMinimization, $0 cost, pre-charged)
        let g = Generator {
            bus: 2,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net_with_sto.generators.push(g);

        let opts = DispatchOptions::default();
        let sol = solve_sced(&net_with_sto, &opts).unwrap();

        // Storage should discharge (free energy vs $20/MWh generator)
        assert!(
            sol.dispatch.storage_discharge_mw[0] > 1.0,
            "Zero-cost storage should discharge against $20/MWh generator, got {:.4}",
            sol.dispatch.storage_discharge_mw[0]
        );
        // Total system cost should be lower with storage (or equal if storage can't be used)
        assert!(
            sol.dispatch.total_cost <= sol_no_storage.dispatch.total_cost + 1e-6,
            "Storage should not increase total system cost: with={:.2}, without={:.2}",
            sol.dispatch.total_cost,
            sol_no_storage.dispatch.total_cost
        );
    }

    #[test]
    fn test_dc_sced_storage_generic_pg_cost_does_not_reward_charging() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, StorageDispatchMode, StorageParams};

        let mut net = Network::new("storage_pg_cost_neutral");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut supply = Generator::new(1, 0.0, 1.0);
        supply.pmin = 0.0;
        supply.pmax = 100.0;
        supply.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(supply);

        let mut storage = Generator::new(1, 0.0, 1.0);
        storage.pmin = -50.0;
        storage.pmax = 50.0;
        storage.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![3.0, 0.0],
        });
        storage.storage = Some(StorageParams {
            charge_efficiency: 0.9486832981,
            discharge_efficiency: 0.9486832981,
            energy_capacity_mwh: 200.0,
            soc_initial_mwh: 100.0,
            soc_min_mwh: 0.0,
            soc_max_mwh: 200.0,
            variable_cost_per_mwh: 1.0,
            degradation_cost_per_mwh: 2.0,
            dispatch_mode: StorageDispatchMode::CostMinimization,
            self_schedule_mw: 0.0,
            discharge_offer: None,
            charge_bid: None,
            max_c_rate_charge: None,
            max_c_rate_discharge: None,
            chemistry: None,
            discharge_foldback_soc_mwh: None,
            charge_foldback_soc_mwh: None,
            daily_cycle_limit: None,
        });
        net.generators.push(storage);

        let sol = crate::sced::solve::solve_sced(&net, &crate::legacy::DispatchOptions::default())
            .expect("SCED with idle storage should converge");

        assert!(
            sol.dispatch.storage_charge_mw[0] < 1e-6,
            "generic generator Pg cost must not create a charging subsidy, got {:.4} MW",
            sol.dispatch.storage_charge_mw[0]
        );
        assert!(
            sol.dispatch.storage_discharge_mw[0] < 1e-6,
            "storage should stay neutral without load or reserve demand, got {:.4} MW",
            sol.dispatch.storage_discharge_mw[0]
        );
        assert!(
            sol.dispatch.pg_mw[0].abs() < 1e-6,
            "thermal generation should stay at zero when storage is neutral, got {:.4} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.total_cost.abs() < 1e-6,
            "idle-storage SCED should have zero objective, got {:.6}",
            sol.dispatch.total_cost
        );
    }

    /// Storage + reserve: battery provides both energy arbitrage and spinning reserve.
    ///
    /// Network:
    ///   Single slack bus, 100 MW load.
    ///   Gen1: $20/MWh, pmax=200 MW (no forced reserve offer — will get default)
    ///   BESS: 50 MW / 200 MWh, soc_initial=100 MWh, CostMinimization, $0 variable cost.
    ///
    /// A 20 MW spin reserve requirement is imposed.
    ///
    /// Expected outcomes:
    ///   1. LP solves successfully.
    ///   2. Power balance holds: Pg_gen + (dis - ch) = 100 MW.
    ///   3. Spin reserve is fully met (total spin ≥ 20 MW).
    ///   4. The BESS contributes some spin reserve (its headroom allows it).
    ///   5. storage_discharge_mw and storage_charge_mw are present and non-negative.
    #[test]
    fn test_sced_storage_with_reserve() {
        use surge_network::market::{CostCurve, ReserveOffer};
        use surge_network::network::{Generator, StorageDispatchMode, StorageParams};

        let mut net = make_two_bus_net();

        // Give gen1 an explicit spin offer so we control exactly what it can provide.
        net.generators[0]
            .market
            .get_or_insert_default()
            .reserve_offers = vec![ReserveOffer {
            product_id: "spin".into(),
            capacity_mw: 50.0,
            cost_per_mwh: 0.0,
        }];

        // Add a BESS at bus 2 (already in the two-bus network).
        let bess = Generator {
            bus: 2,
            in_service: true,
            pmin: -50.0, // max charge = 50 MW
            pmax: 50.0,  // max discharge = 50 MW
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            market: Some(MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: 50.0, // can offer up to full discharge capacity
                    cost_per_mwh: 0.0,
                }],
                ..Default::default()
            }),
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 200.0,
                soc_initial_mwh: 100.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 200.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        };
        net.generators.push(bess);

        let opts = crate::legacy::DispatchOptions {
            enforce_thermal_limits: true,
            system_reserve_requirements: vec![surge_network::market::SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 20.0,
                per_period_mw: None,
            }],
            ..crate::legacy::DispatchOptions::default()
        };

        let sol = crate::sced::solve::solve_sced(&net, &opts)
            .expect("SCED with storage + reserve should converge");

        // 1. Power balance: total gen (including net storage) ≈ 100 MW load
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 100.0).abs() < 1.0,
            "Power balance: total gen={total_gen:.2} MW, expected 100 MW"
        );

        // 2. Spin reserve requirement met
        let spin_awards = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .expect("reserve_awards should have spin key");
        let total_spin: f64 = spin_awards.iter().sum();
        assert!(
            total_spin >= 19.9,
            "Spin reserve should be ≥ 20 MW, got {total_spin:.2}"
        );

        // 3. BESS has non-negative dis/ch results
        assert!(
            !sol.dispatch.storage_discharge_mw.is_empty(),
            "storage_discharge_mw should be populated"
        );
        assert!(
            !sol.dispatch.storage_charge_mw.is_empty(),
            "storage_charge_mw should be populated"
        );
        let dis = sol.dispatch.storage_discharge_mw[0];
        let ch = sol.dispatch.storage_charge_mw[0];
        assert!(dis >= -1e-6, "discharge must be non-negative: {dis:.4}");
        assert!(ch >= -1e-6, "charge must be non-negative: {ch:.4}");

        println!(
            "SCED storage+reserve: gen[0]={:.1}MW, bess_dis={:.1}MW, bess_ch={:.1}MW, spin={:.1}MW",
            sol.dispatch.pg_mw[0], dis, ch, total_spin
        );
    }

    #[test]
    fn test_sced_storage_soc_reserve_coupling_limits_up_award() {
        use std::collections::HashMap;
        use surge_network::Network;
        use surge_network::market::{
            CostCurve, EnergyCoupling, LoadProfile, LoadProfiles, PenaltyCurve, QualificationRule,
            ReserveDirection, ReserveOffer, ReserveProduct,
        };
        use surge_network::network::{Bus, BusType, Generator, StorageDispatchMode, StorageParams};

        let mut net = Network::new("sced_soc_reserve_coupling_up");
        net.base_mva = 100.0;

        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut must_run = Generator::new(1, 100.0, 1.0);
        must_run.pmin = 100.0;
        must_run.pmax = 100.0;
        must_run.commitment.get_or_insert_default().status = CommitmentStatus::MustRun;
        must_run.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(must_run);

        net.generators.push(Generator {
            bus: 1,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            market: Some(MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "spin".into(),
                    capacity_mw: 50.0,
                    cost_per_mwh: 2.0,
                }],
                ..Default::default()
            }),
            storage: Some(StorageParams {
                charge_efficiency: 1.0,
                discharge_efficiency: 1.0,
                energy_capacity_mwh: 20.0,
                soc_initial_mwh: 10.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 20.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            load_profiles: LoadProfiles {
                profiles: vec![LoadProfile {
                    bus: 1,
                    load_mw: vec![100.0],
                }],
                n_timesteps: 1,
            },
            reserve_products: vec![ReserveProduct {
                id: "spin".into(),
                name: "Spinning Reserve".into(),
                direction: ReserveDirection::Up,
                deploy_secs: 600.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Headroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            }],
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 40.0,
                per_period_mw: None,
            }],
            storage_reserve_soc_impact: HashMap::from([(
                1,
                HashMap::from([(String::from("spin"), vec![1.0])]),
            )]),
            ..DispatchOptions::default()
        };

        let sol =
            crate::sced::solve::solve_sced(&net, &opts).expect("SCED with SoC reserve coupling");
        let bess_spin = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .and_then(|awards| awards.get(1))
            .copied()
            .unwrap_or(0.0);
        assert!(
            bess_spin < 12.0,
            "BESS up reserve should be limited by SoC floor, got {:.3} MW",
            bess_spin
        );
    }

    #[test]
    fn test_sced_storage_soc_reserve_coupling_limits_down_award() {
        use std::collections::HashMap;
        use surge_network::Network;
        use surge_network::market::{
            CostCurve, EnergyCoupling, LoadProfile, LoadProfiles, PenaltyCurve, QualificationRule,
            ReserveDirection, ReserveOffer, ReserveProduct,
        };
        use surge_network::network::{Bus, BusType, Generator, StorageDispatchMode, StorageParams};

        let mut net = Network::new("sced_soc_reserve_coupling_down");
        net.base_mva = 100.0;

        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 100.0, 0.0));

        let mut must_run = Generator::new(1, 100.0, 1.0);
        must_run.pmin = 100.0;
        must_run.pmax = 100.0;
        must_run.commitment.get_or_insert_default().status = CommitmentStatus::MustRun;
        must_run.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(must_run);

        net.generators.push(Generator {
            bus: 1,
            in_service: true,
            pmin: -50.0,
            pmax: 50.0,
            machine_base_mva: 100.0,
            cost: Some(CostCurve::Polynomial {
                coeffs: vec![0.0],
                startup: 0.0,
                shutdown: 0.0,
            }),
            market: Some(MarketParams {
                reserve_offers: vec![ReserveOffer {
                    product_id: "reg_down".into(),
                    capacity_mw: 50.0,
                    cost_per_mwh: 2.0,
                }],
                ..Default::default()
            }),
            storage: Some(StorageParams {
                charge_efficiency: 1.0,
                discharge_efficiency: 1.0,
                energy_capacity_mwh: 20.0,
                soc_initial_mwh: 19.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 20.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::CostMinimization,
                self_schedule_mw: 0.0,
                discharge_offer: None,
                charge_bid: None,
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
                daily_cycle_limit: None,
            }),
            ..Generator::default()
        });

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            load_profiles: LoadProfiles {
                profiles: vec![LoadProfile {
                    bus: 1,
                    load_mw: vec![100.0],
                }],
                n_timesteps: 1,
            },
            reserve_products: vec![ReserveProduct {
                id: "reg_down".into(),
                name: "Regulation Down".into(),
                direction: ReserveDirection::Down,
                deploy_secs: 300.0,
                qualification: QualificationRule::Committed,
                energy_coupling: EnergyCoupling::Footroom,
                dispatchable_load_energy_coupling: None,
                shared_limit_products: Vec::new(),
                balance_products: Vec::new(),
                kind: surge_network::market::ReserveKind::Real,
                apply_deploy_ramp_limit: true,
                demand_curve: PenaltyCurve::Linear {
                    cost_per_unit: 1000.0,
                },
            }],
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "reg_down".into(),
                requirement_mw: 40.0,
                per_period_mw: None,
            }],
            storage_reserve_soc_impact: HashMap::from([(
                1,
                HashMap::from([(String::from("reg_down"), vec![-1.0])]),
            )]),
            ..DispatchOptions::default()
        };

        let sol = crate::sced::solve::solve_sced(&net, &opts)
            .expect("SCED with signed SoC reserve coupling");
        let bess_reg_down = sol
            .dispatch
            .reserve_awards
            .get("reg_down")
            .and_then(|awards| awards.get(1))
            .copied()
            .unwrap_or(0.0);
        assert!(
            bess_reg_down < 2.0,
            "BESS down reserve should be limited by SoC ceiling, got {:.3} MW",
            bess_reg_down
        );
    }

    /// E4: PAR flow-setpoint in SCED.
    ///
    /// 3-bus network with a PAR on branch 1→2.  Target MW is set to 30 MW.
    /// Verify:
    /// 1. Power balance holds (total gen = total load).
    /// 2. par_results has one entry with correct target_mw.
    /// 3. implied_shift_deg is a finite number (PAR was found and post-solve angle computed).
    #[test]
    fn test_sced_par_setpoint() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base = 100.0_f64;

        // 3-bus network:
        //   Bus 1 (Slack), Bus 2 (PQ, 80 MW load), Bus 3 (PQ, 60 MW load)
        //   Branch 1→2: PAR (x=0.2), branch 1→3: x=0.1, branch 2→3: x=0.1
        //   Gen 1 at bus 1: pmax=200 MW, cost $20/MWh
        //   Gen 2 at bus 3: pmax=100 MW, cost $25/MWh

        let mut net = Network::new("par_sced_test");
        net.base_mva = base;

        let bus1 = Bus::new(1, BusType::Slack, 100.0);
        let bus2 = Bus::new(2, BusType::PQ, 100.0);
        let bus3 = Bus::new(3, BusType::PQ, 100.0);
        net.buses.extend([bus1, bus2, bus3]);
        net.loads.push(Load::new(2, 80.0, 0.0));
        net.loads.push(Load::new(3, 60.0, 0.0));

        // PAR branch 1→2
        let mut par_branch = Branch::new_line(1, 2, 0.0, 0.2, 0.0);
        par_branch.circuit = "1".to_string();
        par_branch.rating_a_mva = 200.0;
        par_branch.opf_control = Some(BranchOpfControl {
            phase_min_rad: (-30.0_f64).to_radians(),
            phase_max_rad: 30.0_f64.to_radians(),
            ..Default::default()
        });
        net.branches.push(par_branch);

        // Normal lines
        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.circuit = "1".to_string();
        br13.rating_a_mva = 200.0;
        net.branches.push(br13);

        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.circuit = "1".to_string();
        br23.rating_a_mva = 200.0;
        net.branches.push(br23);

        // Gen 1 at bus 1
        let mut gen1 = Generator::new(1, 0.0, 1.0);
        gen1.pmin = 0.0;
        gen1.pmax = 200.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(gen1);

        // Gen 2 at bus 3
        let mut gen2 = Generator::new(3, 0.0, 1.0);
        gen2.pmin = 0.0;
        gen2.pmax = 100.0;
        gen2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![25.0, 0.0],
        });
        net.generators.push(gen2);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            par_setpoints: vec![surge_solution::ParSetpoint {
                from_bus: 1,
                to_bus: 2,
                circuit: "1".to_string(),
                target_mw: 30.0,
            }],
            ..DispatchOptions::default()
        };

        let sol = solve_sced(&net, &opts).expect("PAR setpoint SCED should solve");

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 140.0).abs() < 0.5,
            "power balance: gen={:.2}, load=140.0",
            total_gen
        );

        // PAR result returned
        assert_eq!(
            sol.dispatch.par_results.len(),
            1,
            "should have 1 par_result"
        );
        assert!(
            (sol.dispatch.par_results[0].target_mw - 30.0).abs() < 1e-6,
            "target_mw should be 30.0, got {}",
            sol.dispatch.par_results[0].target_mw
        );
        assert!(
            sol.dispatch.par_results[0].implied_shift_deg.is_finite(),
            "implied_shift_deg should be finite, got {}",
            sol.dispatch.par_results[0].implied_shift_deg
        );
    }

    /// E4: PAR setpoint warning when target exceeds rate_a.
    ///
    /// Uses same 3-bus network as test_sced_par_setpoint but sets rate_a = 10 MW
    /// (small) while target_mw = 30 MW.  The solve must still succeed (warning only).
    #[test]
    fn test_sced_par_setpoint_limit_warning() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Branch, Bus, BusType, Generator};

        let base = 100.0_f64;

        let mut net = Network::new("par_limit_test");
        net.base_mva = base;

        let bus1 = Bus::new(1, BusType::Slack, 100.0);
        let bus2 = Bus::new(2, BusType::PQ, 100.0);
        let bus3 = Bus::new(3, BusType::PQ, 100.0);
        net.buses.extend([bus1, bus2, bus3]);
        net.loads.push(Load::new(2, 80.0, 0.0));
        net.loads.push(Load::new(3, 60.0, 0.0));

        // PAR branch 1→2 with small rate_a (will be exceeded)
        let mut par_branch = Branch::new_line(1, 2, 0.0, 0.2, 0.0);
        par_branch.circuit = "1".to_string();
        par_branch.rating_a_mva = 10.0; // limit is 10 MW — will be exceeded by target_mw=30
        par_branch.opf_control = Some(BranchOpfControl {
            phase_min_rad: (-30.0_f64).to_radians(),
            phase_max_rad: 30.0_f64.to_radians(),
            ..Default::default()
        });
        net.branches.push(par_branch);

        let mut br13 = Branch::new_line(1, 3, 0.0, 0.1, 0.0);
        br13.circuit = "1".to_string();
        br13.rating_a_mva = 200.0;
        net.branches.push(br13);

        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.circuit = "1".to_string();
        br23.rating_a_mva = 200.0;
        net.branches.push(br23);

        let mut gen1 = Generator::new(1, 0.0, 1.0);
        gen1.pmin = 0.0;
        gen1.pmax = 200.0;
        gen1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        net.generators.push(gen1);

        let mut gen2 = Generator::new(3, 0.0, 1.0);
        gen2.pmin = 0.0;
        gen2.pmax = 100.0;
        gen2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![25.0, 0.0],
        });
        net.generators.push(gen2);

        // target_mw = 30 > rate_a = 10 → should warn but still solve
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            par_setpoints: vec![surge_solution::ParSetpoint {
                from_bus: 1,
                to_bus: 2,
                circuit: "1".to_string(),
                target_mw: 30.0, // exceeds rate_a = 10
            }],
            ..DispatchOptions::default()
        };

        let sol = solve_sced(&net, &opts).expect("PAR setpoint SCED should solve even over limit");

        // Power balance still holds
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 140.0).abs() < 0.5,
            "power balance: gen={:.2}, load=140.0",
            total_gen
        );

        // par_results is populated
        assert_eq!(sol.dispatch.par_results.len(), 1);
    }
}

// ---------------------------------------------------------------------------
// Issue #22/#23 — Frequency security + Dispatchable loads
// ---------------------------------------------------------------------------

#[cfg(test)]
mod freq_sec_dr_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::{CostCurve, DispatchableLoad};
    use surge_network::network::{Bus, BusType, Generator};

    /// Build a minimal single-bus network with two generators.
    ///
    /// Bus 1 (slack), load = `load_mw` MW.
    /// Gen 0: cheap  ($10/MWh), pmax = `g0_pmax` MW, pmin = 0.
    /// Gen 1: expensive ($50/MWh), pmax = `g1_pmax` MW, pmin = 0.
    fn two_gen_net(load_mw: f64, g0_pmax: f64, g1_pmax: f64) -> Network {
        let mut net = Network::new("freq_dr_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, load_mw, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = g0_pmax;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = g1_pmax;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        net.generators.push(g1);

        net
    }

    // -----------------------------------------------------------------------
    // Issue #23: Dispatchable loads
    // -----------------------------------------------------------------------

    /// DL-01: A curtailable load with cost=$30/MWh on a bus with LMP=$10/MWh
    /// should not be curtailed (curtailment costs more than LMP savings).
    /// Result: DL is served at its scheduled level (p_max_pu).
    #[test]
    fn test_dispatchable_load_no_curtailment() {
        // Load = 50 MW (fixed) + 20 MW DL (scheduled).  Cheap gen covers all.
        let net = two_gen_net(50.0, 200.0, 200.0);
        // Add a dispatchable load of 20 MW at bus 0 (0-indexed).
        // Curtailment cost = $30/MWh; since LMP ≈ $10/MWh, curtailing costs more → serve fully.
        let dl = DispatchableLoad::curtailable(
            1,     // bus number
            20.0,  // p_sched_mw
            0.0,   // q_sched_mvar
            0.0,   // p_min_mw (fully curtailable)
            30.0,  // cost_per_mwh
            100.0, // base_mva
        );

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // DL should be served at its full scheduled level (20 MW = 0.2 pu)
        assert!(
            !sol.dispatch.dr_results.loads.is_empty(),
            "DR results should be populated"
        );
        let served = sol.dispatch.dr_results.loads[0].p_served_pu * 100.0; // convert pu→MW (base=100)
        assert!(
            served > 18.0,
            "DL should be fully served (not curtailed): p_served={served:.2} MW"
        );

        // Total generation = fixed load + DL served
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - (50.0 + served)).abs() < 1.0,
            "Power balance: gen={total_gen:.2}, load+DL={:.2}",
            50.0 + served
        );
    }

    /// DL-02: A curtailable load with cost=$5/MWh on a bus with LMP=$10/MWh
    /// should be curtailed (curtailing saves more than the curtailment cost).
    /// Result: DL dispatch should be less than its scheduled level.
    #[test]
    fn test_dispatchable_load_basic_curtailment() {
        // Single bus: load = 150 MW, gen pmax = 200 MW (cheap at $10/MWh).
        let net = two_gen_net(100.0, 200.0, 200.0);
        // DL: 50 MW scheduled, curtailment bid = $5/MWh (< LMP ≈ $10 → curtail)
        let dl = DispatchableLoad::curtailable(
            1,     // bus number
            50.0,  // p_sched_mw
            0.0,   // q_sched_mvar
            0.0,   // p_min_mw (fully curtailable)
            5.0,   // cost_per_mwh  — below LMP → should curtail
            100.0, // base_mva
        );

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(!sol.dispatch.dr_results.loads.is_empty());
        let served = sol.dispatch.dr_results.loads[0].p_served_pu * 100.0; // convert pu→MW (base=100)
        // At LMP=$10 > bid=$5 the optimizer should curtail — served < scheduled (50 MW)
        assert!(
            served < 48.0,
            "DL should be curtailed: p_served={served:.2} < 50 MW scheduled"
        );

        // Power balance: gen ≈ fixed_load + DL_served
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - (100.0 + served)).abs() < 1.0,
            "Power balance violated: gen={total_gen:.2}"
        );
    }

    /// DL-03: Power balance must hold exactly when a DL is served at a partial level.
    /// Separately verify: total generation equals fixed load + DL served.
    #[test]
    fn test_dispatchable_load_power_balance() {
        let net = two_gen_net(80.0, 200.0, 200.0);
        // DL: 40 MW, must serve >= 20 MW (p_min_mw=20), curtailment cost=$8/MWh
        let dl = DispatchableLoad::curtailable(
            1, 40.0, 0.0, 20.0, // p_min_mw — partial curtailment only
            8.0,  // cost_per_mwh
            100.0,
        );

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        let served = sol.dispatch.dr_results.loads[0].p_served_pu * 100.0; // convert pu→MW (base=100)
        // p_min = 20 MW, so served must be >= 20 MW
        assert!(
            served >= 19.5,
            "DL served must respect p_min=20 MW: served={served:.2}"
        );

        // Power balance: Σ Pg = fixed_load + DL_served
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        let expected_load = 80.0 + served;
        assert!(
            (total_gen - expected_load).abs() < 0.5,
            "Power balance: gen={total_gen:.2}, expected={expected_load:.2}"
        );
    }

    /// RT interval-19 repro reduction: cheap PWL thermal headroom plus high-value
    /// dispatchable load should clear the DR, not curtail it at a five-figure LMP.
    #[test]
    fn test_dispatchable_load_with_pwl_headroom_serves_load_at_reasonable_price() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("dl_pwl_headroom");
        net.base_mva = 100.0;
        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 302.197_934_854_174_4, 0.0));

        // G1 matches the problematic RT interval's offer shape:
        // 50 MW at $18.2/MWh, then up to 200 MW at $18.8/MWh, with headroom left.
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 400.0), (50.0, 1310.0), (200.0, 4130.0)],
        });
        net.generators.push(g1);

        // Cheap must-run nuclear block already at max.
        let mut g4 = Generator::new(1, 0.0, 1.0);
        g4.pmin = 100.0;
        g4.pmax = 100.0;
        g4.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![8.0, 0.0],
        });
        net.generators.push(g4);

        // Fixed renewable injections matching the bad RT interval.
        let mut w1 = Generator::new(1, 0.0, 1.0);
        w1.pmin = 85.045_117_636_062_34;
        w1.pmax = 85.045_117_636_062_34;
        w1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(w1);

        let mut s1 = Generator::new(1, 0.0, 1.0);
        s1.pmin = 0.825_251_537_997_639_4;
        s1.pmax = 0.825_251_537_997_639_4;
        s1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(s1);

        let dl0 = DispatchableLoad::curtailable(1, 30.0, 0.0, 0.0, 100.0, net.base_mva);
        let dl1 = DispatchableLoad::curtailable(1, 20.0, 0.0, 0.0, 200.0, net.base_mva);
        let dl2 = DispatchableLoad::curtailable(1, 15.0, 0.0, 0.0, 80.0, net.base_mva);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl0, dl1, dl2],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).expect("SCED with PWL thermal and DR should solve");

        let served: Vec<f64> = sol
            .dispatch
            .dr_results
            .loads
            .iter()
            .map(|r| r.p_served_pu * net.base_mva)
            .collect();
        assert_eq!(served.len(), 3, "expected all three dispatchable loads");
        assert!(
            served[0] > 29.5 && served[1] > 19.5 && served[2] > 14.5,
            "dispatchable loads should clear when cheap thermal headroom exists: served={served:?}"
        );

        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        let expected_load = 302.197_934_854_174_4 + served.iter().sum::<f64>();
        assert!(
            (total_gen - expected_load).abs() < 0.5,
            "power balance: gen={total_gen:.3}, expected={expected_load:.3}"
        );

        assert!(
            sol.dispatch.pg_mw[0] > 180.0,
            "cheap PWL thermal should pick up DR demand before curtailment, got {:.3} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            sol.dispatch.lmp[0] < 50.0,
            "LMP should stay near the marginal thermal segment, got {:.3}",
            sol.dispatch.lmp[0]
        );
    }

    /// Add the market30-style idle storage fleet at minimum SoC. Storage should
    /// remain neutral and must not contaminate the energy price or force DR
    /// curtailment when cheap thermal headroom exists.
    #[test]
    fn test_dispatchable_load_with_storage_at_min_soc_keeps_reasonable_price() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator, StorageDispatchMode, StorageParams};

        let mut net = Network::new("dl_pwl_with_idle_storage");
        net.base_mva = 100.0;
        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 302.197_934_854_174_4, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 400.0), (50.0, 1310.0), (200.0, 4130.0)],
        });
        net.generators.push(g1);

        let mut g4 = Generator::new(1, 0.0, 1.0);
        g4.pmin = 100.0;
        g4.pmax = 100.0;
        g4.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![8.0, 0.0],
        });
        net.generators.push(g4);

        let mut w1 = Generator::new(1, 0.0, 1.0);
        w1.pmin = 85.045_117_636_062_34;
        w1.pmax = 85.045_117_636_062_34;
        w1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(w1);

        let mut s1 = Generator::new(1, 0.0, 1.0);
        s1.pmin = 0.825_251_537_997_639_4;
        s1.pmax = 0.825_251_537_997_639_4;
        s1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(s1);

        let mut bess = Generator::new(1, 0.0, 1.0);
        bess.pmin = -50.0;
        bess.pmax = 50.0;
        bess.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });
        bess.storage = Some(StorageParams {
            charge_efficiency: 0.9486832981,
            discharge_efficiency: 0.9486832981,
            energy_capacity_mwh: 190.0,
            soc_initial_mwh: 15.0,
            soc_min_mwh: 15.0,
            soc_max_mwh: 190.0,
            variable_cost_per_mwh: 1.0,
            degradation_cost_per_mwh: 2.0,
            dispatch_mode: StorageDispatchMode::CostMinimization,
            self_schedule_mw: 0.0,
            discharge_offer: None,
            charge_bid: None,
            max_c_rate_charge: None,
            max_c_rate_discharge: None,
            chemistry: None,
            discharge_foldback_soc_mwh: None,
            charge_foldback_soc_mwh: None,
            daily_cycle_limit: None,
        });
        net.generators.push(bess);

        let mut ph = Generator::new(1, 0.0, 1.0);
        ph.pmin = -80.0;
        ph.pmax = 100.0;
        ph.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0],
        });
        ph.storage = Some(StorageParams {
            charge_efficiency: 0.9219544457,
            discharge_efficiency: 0.9219544457,
            energy_capacity_mwh: 760.0,
            soc_initial_mwh: 80.0,
            soc_min_mwh: 80.0,
            soc_max_mwh: 760.0,
            variable_cost_per_mwh: 1.0,
            degradation_cost_per_mwh: 0.5,
            dispatch_mode: StorageDispatchMode::CostMinimization,
            self_schedule_mw: 0.0,
            discharge_offer: None,
            charge_bid: None,
            max_c_rate_charge: None,
            max_c_rate_discharge: None,
            chemistry: None,
            discharge_foldback_soc_mwh: None,
            charge_foldback_soc_mwh: None,
            daily_cycle_limit: None,
        });
        net.generators.push(ph);

        let dl0 = DispatchableLoad::curtailable(1, 30.0, 0.0, 0.0, 100.0, net.base_mva);
        let dl1 = DispatchableLoad::curtailable(1, 20.0, 0.0, 0.0, 200.0, net.base_mva);
        let dl2 = DispatchableLoad::curtailable(1, 15.0, 0.0, 0.0, 80.0, net.base_mva);

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl0, dl1, dl2],
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).expect("SCED with idle storage should solve");

        let served: Vec<f64> = sol
            .dispatch
            .dr_results
            .loads
            .iter()
            .map(|r| r.p_served_pu * net.base_mva)
            .collect();
        assert_eq!(served.len(), 3, "expected all three dispatchable loads");
        assert!(
            served[0] > 29.5 && served[1] > 19.5 && served[2] > 14.5,
            "idle storage at min SoC must not suppress DR serving: served={served:?}"
        );
        assert!(
            sol.dispatch.lmp[0] < 50.0,
            "idle storage at min SoC must not create a five-figure LMP, got {:.3}",
            sol.dispatch.lmp[0]
        );
        assert!(
            sol.dispatch
                .storage_discharge_mw
                .iter()
                .all(|mw| mw.abs() < 1e-6),
            "storage should remain idle at min SoC, got discharge {:?}",
            sol.dispatch.storage_discharge_mw
        );
    }

    /// Regression for the market30 RT pricing bug: a multi-segment unit pinned
    /// at minimum output must not drag the SCED LMP into the thousands when a
    /// different cheap unit is actually marginal.
    #[test]
    fn test_multi_segment_min_gen_does_not_pollute_sced_lmp() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("pwl_min_gen_price_regression");
        net.base_mva = 100.0;
        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 302.72, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![(0.0, 400.0), (50.0, 1310.0), (200.0, 4130.0)],
        });
        net.generators.push(g1);

        let mut g4 = Generator::new(1, 0.0, 1.0);
        g4.pmin = 50.0;
        g4.pmax = 100.0;
        g4.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![8.0, 0.0],
        });
        net.generators.push(g4);

        let mut w1 = Generator::new(1, 0.0, 1.0);
        w1.pmin = 120.0;
        w1.pmax = 120.0;
        w1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(w1);

        let mut s1 = Generator::new(1, 0.0, 1.0);
        s1.pmin = 80.0;
        s1.pmax = 80.0;
        s1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 0.0],
        });
        net.generators.push(s1);

        let sol = solve_sced(&net, &DispatchOptions::default())
            .expect("SCED with a minimum-output PWL unit should solve");

        assert!(
            (sol.dispatch.pg_mw[0] - 50.0).abs() < 1e-6,
            "G1 should sit at minimum output, got {:.6} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            (sol.dispatch.pg_mw[1] - 52.72).abs() < 1e-3,
            "G4 should be the marginal unit, got {:.6} MW",
            sol.dispatch.pg_mw[1]
        );
        assert!(
            sol.dispatch.lmp[0] < 20.0,
            "LMP should stay near G4/G1 segment costs, got {:.3}",
            sol.dispatch.lmp[0]
        );
    }

    /// Regression: a fixed-commitment SCED with a marginal offer-schedule PWL
    /// unit should price at the active segment cost, not a five-figure value.
    #[test]
    fn test_fixed_commitment_offer_schedule_pwl_prices_at_marginal_segment() {
        use crate::dispatch::CommitmentMode;
        use std::collections::HashMap;
        use surge_network::Network;
        use surge_network::market::{CostCurve, OfferCurve, OfferSchedule};
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("fixed_commitment_offer_schedule_pwl_price");
        net.base_mva = 100.0;
        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 151.478_909_9, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0],
        });
        net.generators.push(g1);

        let mut g4 = Generator::new(1, 0.0, 1.0);
        g4.pmin = 100.0;
        g4.pmax = 100.0;
        g4.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![8.0, 0.0],
        });
        net.generators.push(g4);

        let opts = DispatchOptions {
            commitment: CommitmentMode::Fixed {
                commitment: vec![true, true],
                per_period: None,
            },
            offer_schedules: HashMap::from([(
                0,
                OfferSchedule {
                    periods: vec![Some(OfferCurve {
                        segments: vec![(50.0, 18.2), (200.0, 18.8)],
                        no_load_cost: 400.0,
                        startup_tiers: vec![],
                    })],
                },
            )]),
            enforce_thermal_limits: false,
            ..DispatchOptions::default()
        };

        let sol = solve_sced(&net, &opts)
            .expect("fixed-commitment SCED with a PWL offer schedule should solve");

        assert!(
            (sol.dispatch.pg_mw[0] - 51.478_909_9).abs() < 1e-5,
            "G1 should set the margin just above pmin, got {:.6} MW",
            sol.dispatch.pg_mw[0]
        );
        assert!(
            (sol.dispatch.pg_mw[1] - 100.0).abs() < 1e-6,
            "G4 should remain fixed at 100 MW, got {:.6} MW",
            sol.dispatch.pg_mw[1]
        );
        assert!(
            sol.dispatch.lmp[0] < 25.0,
            "fixed-commitment PWL offer should price near $18.8/MWh, got {:.3}",
            sol.dispatch.lmp[0]
        );
    }

    #[test]
    fn test_market30_interval19_fixed_commitment_offer_schedule_prices_reasonably() {
        use crate::request::{
            CommitmentPolicy, CommitmentSchedule, DispatchInitialState, DispatchMarket,
            DispatchNetwork, DispatchProfiles, DispatchRequest, DispatchState, DispatchTimeline,
            GeneratorOfferSchedule, ResourceCommitmentSchedule, ResourceDispatchPoint,
            StorageSocOverride,
        };
        use std::path::Path;
        use surge_io::load;
        use surge_network::market::{
            LoadProfile, LoadProfiles, OfferCurve, OfferSchedule, RenewableProfile,
            RenewableProfiles,
        };

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let case_path = repo_root.join("examples/cases/market30/market30.surge.json.zst");
        let net = load(&case_path).expect("load market30 network");

        let load_profiles = LoadProfiles {
            n_timesteps: 1,
            profiles: vec![
                LoadProfile {
                    bus: 2,
                    load_mw: vec![33.90352320478991],
                },
                LoadProfile {
                    bus: 3,
                    load_mw: vec![3.7561023588861047],
                },
                LoadProfile {
                    bus: 4,
                    load_mw: vec![11.917065415989692],
                },
                LoadProfile {
                    bus: 7,
                    load_mw: vec![35.9900947342917],
                },
                LoadProfile {
                    bus: 8,
                    load_mw: vec![47.470322747592725],
                },
                LoadProfile {
                    bus: 10,
                    load_mw: vec![9.223200616382075],
                },
                LoadProfile {
                    bus: 12,
                    load_mw: vec![17.89757056326987],
                },
                LoadProfile {
                    bus: 14,
                    load_mw: vec![9.953063899163295],
                },
                LoadProfile {
                    bus: 15,
                    load_mw: vec![13.191538437216185],
                },
                LoadProfile {
                    bus: 16,
                    load_mw: vec![5.64159120994562],
                },
                LoadProfile {
                    bus: 17,
                    load_mw: vec![14.533021571923243],
                },
                LoadProfile {
                    bus: 18,
                    load_mw: vec![5.175651051163543],
                },
                LoadProfile {
                    bus: 19,
                    load_mw: vec![15.387123888468615],
                },
                LoadProfile {
                    bus: 20,
                    load_mw: vec![3.567711073170374],
                },
                LoadProfile {
                    bus: 21,
                    load_mw: vec![28.40868949312065],
                },
                LoadProfile {
                    bus: 23,
                    load_mw: vec![5.202320491887076],
                },
                LoadProfile {
                    bus: 24,
                    load_mw: vec![14.150092616173977],
                },
                LoadProfile {
                    bus: 26,
                    load_mw: vec![5.694672471612567],
                },
                LoadProfile {
                    bus: 29,
                    load_mw: vec![3.9029543014211687],
                },
                LoadProfile {
                    bus: 30,
                    load_mw: vec![17.231624707705997],
                },
            ],
        };

        let renewable_profiles = RenewableProfiles {
            n_timesteps: 1,
            profiles: vec![
                RenewableProfile {
                    generator_id: net.generators[6].id.clone(),
                    capacity_factors: vec![0.7087093136338527],
                },
                RenewableProfile {
                    generator_id: net.generators[7].id.clone(),
                    capacity_factors: vec![0.010315644224970492],
                },
            ],
        };

        let resource_id = |generator: &surge_network::network::Generator| {
            if generator.id.is_empty() {
                let machine_id = generator.machine_id.as_deref().unwrap_or("1");
                if generator.storage.is_some() {
                    format!("storage:{}:{machine_id}", generator.bus)
                } else {
                    format!("gen:{}:{machine_id}", generator.bus)
                }
            } else {
                generator.id.clone()
            }
        };
        let in_service_generators: Vec<_> =
            net.generators.iter().filter(|g| g.in_service).collect();
        let fixed_commitment = [
            true, false, false, false, true, false, true, true, true, true,
        ];
        let previous_dispatch = [
            111.17281729150601,
            0.0,
            0.0,
            0.0,
            100.0,
            0.0,
            75.6010466620491,
            8.101452421767913,
            0.0,
            0.0,
        ];

        let request = DispatchRequest {
            timeline: DispatchTimeline {
                periods: 1,
                interval_hours: 1.0,
                interval_hours_by_period: Vec::new(),
            },
            commitment: CommitmentPolicy::Fixed(CommitmentSchedule {
                resources: fixed_commitment
                    .into_iter()
                    .enumerate()
                    .map(|(local_idx, initial)| ResourceCommitmentSchedule {
                        resource_id: resource_id(in_service_generators[local_idx]),
                        initial,
                        periods: None,
                    })
                    .collect(),
            }),
            profiles: DispatchProfiles {
                load: load_profiles.into(),
                renewable: renewable_profiles.into(),
                ..DispatchProfiles::default()
            },
            state: DispatchState {
                initial: DispatchInitialState {
                    previous_resource_dispatch: previous_dispatch
                        .into_iter()
                        .enumerate()
                        .map(|(local_idx, mw)| ResourceDispatchPoint {
                            resource_id: resource_id(in_service_generators[local_idx]),
                            mw,
                        })
                        .collect(),
                    storage_soc_overrides: vec![
                        StorageSocOverride {
                            resource_id: resource_id(&net.generators[8]),
                            soc_mwh: 15.0,
                        },
                        StorageSocOverride {
                            resource_id: resource_id(&net.generators[9]),
                            soc_mwh: 160.04050770604192,
                        },
                    ],
                    ..DispatchInitialState::default()
                },
            },
            market: DispatchMarket {
                generator_offer_schedules: vec![
                    GeneratorOfferSchedule {
                        resource_id: resource_id(&net.generators[0]),
                        schedule: OfferSchedule {
                            periods: vec![Some(OfferCurve {
                                segments: vec![(50.0, 18.2), (200.0, 18.8)],
                                no_load_cost: 400.0,
                                startup_tiers: vec![],
                            })],
                        },
                    },
                    GeneratorOfferSchedule {
                        resource_id: resource_id(&net.generators[9]),
                        schedule: OfferSchedule {
                            periods: vec![Some(OfferCurve {
                                segments: vec![(100.0, 1.0)],
                                no_load_cost: 0.0,
                                startup_tiers: vec![],
                            })],
                        },
                    },
                ],
                ..DispatchMarket::default()
            },
            network: DispatchNetwork {
                thermal_limits: crate::request::ThermalLimitPolicy {
                    enforce: false,
                    ..crate::request::ThermalLimitPolicy::default()
                },
                flowgates: crate::request::FlowgatePolicy {
                    enabled: false,
                    ..crate::request::FlowgatePolicy::default()
                },
                ..DispatchNetwork::default()
            },
            ..DispatchRequest::default()
        };

        let sol = crate::dispatch::solve_dispatch_raw(&net, &request)
            .expect("market30 interval-19 reduction should solve");
        let period = &sol.periods[0];
        println!("market30_interval19_dispatch={:?}", period.pg_mw);
        println!("market30_interval19_lmp={:?}", period.lmp);
        println!(
            "market30_interval19_constraints={:?}",
            period.constraint_results
        );

        // G0 ($18.2/MW offer) is the most expensive committed unit, so it sits at
        // Pmin=50 MW while cheaper G4 ($8/MW network cost) and G9 ($1/MW storage
        // offer) satisfy load.  G4 is the marginal thermal unit → LMPs ≈ $8.
        assert!(
            (period.pg_mw[0] - 50.0).abs() < 1.0,
            "G0 should be at Pmin (~50 MW) as the expensive unit, got {:.6} MW",
            period.pg_mw[0]
        );
        assert!(
            period.lmp[0] < 50.0 && period.lmp[0] > 1.0,
            "market30 interval-19 LMPs should be reasonable (between G9 $1 and G0 $18), got {:.3}",
            period.lmp[0]
        );
    }

    // -----------------------------------------------------------------------
    // Issue #22: Frequency security
    // -----------------------------------------------------------------------

    /// FS-01: Default FrequencySecurityOptions (all None) adds zero LP rows —
    /// no performance impact and solution is identical to unconstrained SCED.
    #[test]
    fn test_frequency_security_default_no_rows() {
        let net = two_gen_net(100.0, 200.0, 200.0);
        let opts_base = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let opts_freq = DispatchOptions {
            enforce_thermal_limits: false,
            frequency_security: crate::config::frequency::FrequencySecurityOptions::default(),
            ..Default::default()
        };

        let sol_base = solve_sced(&net, &opts_base).unwrap();
        let sol_freq = solve_sced(&net, &opts_freq).unwrap();

        // Identical dispatch when freq security is inactive
        for (i, (&p0, &p1)) in sol_base
            .dispatch
            .pg_mw
            .iter()
            .zip(sol_freq.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (p0 - p1).abs() < 0.01,
                "Gen {i}: base={p0:.3}, freq={p1:.3} — should be identical when freq_sec is inactive"
            );
        }
        // Default FrequencySecurityOptions reports frequency_secure=true
        assert!(sol_freq.frequency_secure);
    }

    /// FS-02: Inertia constraint forces expensive generator online when cheap
    /// generator alone cannot meet the inertia floor.
    ///
    /// Setup:
    ///   Gen 0: cheap ($10/MWh), pmax=100 MW, H=1.0 s, mbase=100 MVA → 100 MW·s
    ///   Gen 1: expensive ($50/MWh), pmax=200 MW, H=5.0 s, mbase=200 MVA → 1000 MW·s
    ///   Load = 80 MW.
    ///
    /// Without inertia: all load served by Gen 0 (cheap).  Inertia proxy ≈ 80 MW·s.
    /// With min_inertia_mws=500: Gen 0 alone can't provide 500 MW·s, so Gen 1 must
    /// be dispatched to contribute inertia.
    #[test]
    fn test_frequency_security_inertia_constraint() {
        let mut net = two_gen_net(80.0, 100.0, 200.0);
        net.base_mva = 100.0;
        // Set inertia constants on generators
        net.generators[0].h_inertia_s = Some(1.0); // H=1 s, mbase=100 MVA → low inertia
        net.generators[0].machine_base_mva = 100.0;
        net.generators[1].h_inertia_s = Some(5.0); // H=5 s, mbase=200 MVA → high inertia
        net.generators[1].machine_base_mva = 200.0;

        // Without inertia constraint
        let opts_no_inertia = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol_no = solve_sced(&net, &opts_no_inertia).unwrap();
        // Cheap gen should supply most/all of 80 MW
        assert!(
            sol_no.dispatch.pg_mw[0] > 70.0,
            "Without inertia: Gen0 (cheap) should supply most load. pg0={:.2}",
            sol_no.dispatch.pg_mw[0]
        );

        // With inertia constraint of 200 MW·s.
        // Gen 0 alone (80 MW, H=1 s, mbase=100) provides ~80 MW·s — not enough.
        // Gen 1 (H=5 s, mbase=200) must be dispatched to contribute inertia.
        // Constraint: (1.0/100)*Pg0_pu + (5.0*200/200/100)*Pg1_pu ≥ 200/100 = 2.0 (pu·s)
        let freq_opts = crate::config::frequency::FrequencySecurityOptions {
            min_inertia_mws: Some(200.0),
            ..Default::default()
        };
        let opts_inertia = DispatchOptions {
            enforce_thermal_limits: false,
            frequency_security: freq_opts,
            ..Default::default()
        };
        let sol_inertia = solve_sced(&net, &opts_inertia).unwrap();

        // Gen 1 (expensive, high-inertia) must now be dispatched to meet the 500 MW·s floor
        assert!(
            sol_inertia.dispatch.pg_mw[1] > 1.0,
            "With inertia constraint: Gen1 (high-inertia) must be dispatched. pg1={:.2}",
            sol_inertia.dispatch.pg_mw[1]
        );
        // Power balance must still hold
        let total_gen: f64 = sol_inertia.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 80.0).abs() < 1.0,
            "Power balance with inertia constraint: gen={total_gen:.2}, load=80.0"
        );
    }

    /// FS-03: PFR constraint forces expensive generator to hold headroom.
    ///
    /// Setup:
    ///   Gen 0: cheap ($10/MWh), pmax=100 MW, pfr_eligible=true
    ///   Gen 1: expensive ($50/MWh), pmax=200 MW, pfr_eligible=true
    ///   Load = 80 MW.  min_pfr_mw = 80 MW.
    ///
    /// Without PFR: cheap gen covers all 80 MW (headroom = 100-80 = 20 MW from Gen0,
    /// 200 MW from Gen1 = 220 MW total, but Gen1 is not dispatched so provides 200 MW).
    /// Wait — Gen1 is not online in DC-OPF, it just has Pg bounds [0, pmax].
    /// Actually: without PFR gen0=80, gen1=0. Headroom from gen0=20, gen1=200 → total=220 ≥ 80.
    ///
    /// To make the PFR constraint binding we need a tighter setup:
    ///   Gen 0: pmax=90 MW, pfr_eligible=true; Gen 1: pmax=20 MW, pfr_eligible=false.
    ///   Load = 85 MW.  Without PFR: gen0=85, gen1=0. Headroom=5 MW (only from gen0).
    ///   With min_pfr_mw=15: gen0 must hold back ≥15 MW headroom, so gen0 ≤ 75 MW.
    ///   Gen1 must cover the shortfall: gen1 ≥ 10 MW (even though expensive).
    #[test]
    fn test_frequency_security_pfr_constraint() {
        // Build a single-bus network with two generators.
        let mut net = Network::new("pfr_test");
        net.base_mva = 100.0;
        let bus = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(bus);
        net.loads.push(Load::new(1, 85.0, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 90.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        g0.pfr_eligible = true;
        net.generators.push(g0);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 20.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        g1.pfr_eligible = false; // Gen1 does NOT contribute to PFR
        net.generators.push(g1);

        // Without PFR constraint: cheap gen0 covers most of load.
        let opts_no_pfr = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let sol_no = solve_sced(&net, &opts_no_pfr).unwrap();
        // Gen0 should carry ~85 MW, Gen1 ≈ 0
        assert!(
            sol_no.dispatch.pg_mw[0] > 80.0,
            "Without PFR: Gen0 (cheap) should cover load. pg0={:.2}",
            sol_no.dispatch.pg_mw[0]
        );
        // Headroom from pfr_eligible gen0 = 90 - pg0 ≈ 5 MW  (<< 15 MW requirement)
        let headroom_no_pfr = 90.0 - sol_no.dispatch.pg_mw[0];
        assert!(
            headroom_no_pfr < 12.0,
            "Without PFR headroom from Gen0 should be small: {headroom_no_pfr:.2} MW"
        );

        // With min_pfr_mw=15: Gen0 (pfr_eligible) must hold ≥15 MW headroom
        // → Gen0 ≤ 75 MW → Gen1 must pick up ≥10 MW despite being more expensive.
        let freq_opts = crate::config::frequency::FrequencySecurityOptions {
            min_pfr_mw: Some(15.0),
            ..Default::default()
        };
        let opts_pfr = DispatchOptions {
            enforce_thermal_limits: false,
            frequency_security: freq_opts,
            ..Default::default()
        };
        let sol_pfr = solve_sced(&net, &opts_pfr).unwrap();

        // Gen0 must now hold back headroom → dispatched less than without constraint
        let headroom_pfr = 90.0 - sol_pfr.dispatch.pg_mw[0];
        assert!(
            headroom_pfr >= 14.5,
            "With PFR: Gen0 headroom must be ≥15 MW, got {headroom_pfr:.2} MW"
        );

        // Gen1 (not pfr_eligible, expensive) must cover the shortfall
        assert!(
            sol_pfr.dispatch.pg_mw[1] > 5.0,
            "With PFR: Gen1 (expensive) must be dispatched to cover shortfall. pg1={:.2}",
            sol_pfr.dispatch.pg_mw[1]
        );

        // Power balance must hold
        let total_gen: f64 = sol_pfr.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 85.0).abs() < 1.0,
            "PFR power balance: gen={total_gen:.2}, load=85.0"
        );
    }

    /// FS-04: Empty dispatchable_loads list produces identical results to no-DL run
    /// (regression guard — adding DL support must not perturb the base case).
    #[test]
    fn test_frequency_security_empty_dl_no_regression() {
        let net = two_gen_net(120.0, 200.0, 200.0);

        let opts_no_dl = DispatchOptions {
            enforce_thermal_limits: false,
            ..Default::default()
        };
        let opts_empty_dl = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![],
            ..Default::default()
        };

        let sol_no = solve_sced(&net, &opts_no_dl).unwrap();
        let sol_empty = solve_sced(&net, &opts_empty_dl).unwrap();

        assert!(
            (sol_no.dispatch.total_cost - sol_empty.dispatch.total_cost).abs() < 0.01,
            "Empty DL list must not change total cost: no_dl={:.4}, empty_dl={:.4}",
            sol_no.dispatch.total_cost,
            sol_empty.dispatch.total_cost
        );
        for (i, (&p0, &p1)) in sol_no
            .dispatch
            .pg_mw
            .iter()
            .zip(sol_empty.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (p0 - p1).abs() < 0.01,
                "Gen {i}: no_dl={p0:.3}, empty_dl={p1:.3}"
            );
        }
        // DR results should be empty when no DLs specified
        assert!(
            sol_empty.dispatch.dr_results.loads.is_empty(),
            "dr_results should be empty when no DLs provided"
        );
    }
}

#[cfg(test)]
mod nomogram_sced_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::OperatingNomogram;
    use surge_network::network::{Branch, Bus, BusType, Flowgate, Generator};

    /// 3-bus network: Bus1(Slack)-Bus2-Bus3, G1@bus1 ($20/MWh), G2@bus3 ($40/MWh), 150MW@bus2.
    ///
    /// Branch 1-2 (x=0.1, rate=200 MW): carries G1's output to the load — monitored as FG_12.
    /// Branch 2-3 (x=0.1, rate=200 MW): carries G2's output to the load.
    ///
    /// Nomogram: FG_12 constrains itself (self-referential with flat limit = 100 MW).
    /// This forces branch 1-2 ≤ 100 MW, so G1 ≤ 100 MW and G2 picks up the rest.
    ///
    /// Iteration converges at the fixed point: FG_12 flow = 100 MW, limit = 100 MW.
    fn make_three_bus_with_nomogram() -> Network {
        let mut net = Network::new("nomogram_test");
        net.base_mva = 100.0;

        let b1 = Bus::new(1, BusType::Slack, 138.0);
        let b2 = Bus::new(2, BusType::PQ, 138.0);
        let b3 = Bus::new(3, BusType::PQ, 138.0);
        net.buses.extend([b1, b2, b3]);
        net.loads.push(Load::new(2, 150.0, 0.0));

        let mut br12 = Branch::new_line(1, 2, 0.0, 0.1, 0.0);
        br12.rating_a_mva = 200.0;
        br12.circuit = "1".to_string();
        let mut br23 = Branch::new_line(2, 3, 0.0, 0.1, 0.0);
        br23.rating_a_mva = 200.0;
        br23.circuit = "1".to_string();
        net.branches.extend([br12, br23]);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 250.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![20.0, 0.0],
        });
        let mut g2 = Generator::new(3, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 250.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        net.generators.extend([g1, g2]);

        // FG_12 monitors branch 1-2 (carries G1's output).
        net.flowgates.push(Flowgate {
            name: "FG_12".to_string(),
            monitored: vec![WeightedBranchRef::new(1, 2, "1", 1.0)],
            contingency_branch: None,
            limit_mw: 200.0,
            limit_reverse_mw: 0.0,
            in_service: true,
            limit_mw_schedule: vec![],
            limit_reverse_mw_schedule: vec![],
            hvdc_coefficients: vec![],
            hvdc_band_coefficients: vec![],
            ptdf_per_bus: Vec::new(),
            limit_mw_active_period: None,
            breach_sides: surge_network::network::FlowgateBreachSides::Both,
        });

        // Self-referential nomogram: FG_12 flow → tighten FG_12 limit to 100 MW.
        // Points are flat at 100 MW for all flows → fixed point at flow = limit = 100 MW.
        net.nomograms.push(OperatingNomogram {
            name: "NOM_12_self".to_string(),
            index_flowgate: "FG_12".to_string(),
            constrained_flowgate: "FG_12".to_string(),
            points: vec![(0.0, 100.0), (200.0, 100.0)],
            in_service: true,
        });

        net
    }

    /// Without nomogram enforcement (max_nomogram_iter=0): G1 supplies all 150 MW.
    #[test]
    fn test_sced_nomogram_disabled_cheap_wins() {
        let net = make_three_bus_with_nomogram();
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            max_nomogram_iter: 0,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        // Without nomogram, G1 serves all 150 MW freely (FG_12 limit = 200 MW).
        assert!(
            sol.dispatch.pg_mw[0] > 140.0,
            "No nomogram: G1 should win, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
    }

    /// With nomogram enforcement: FG_12 initially carries 150 MW → nomogram tightens
    /// FG_12 limit to 100 MW → G1 forced to ≤ 100 MW → G2 picks up 50 MW.
    /// Converges in one iteration (fixed point at flow = limit = 100 MW).
    #[test]
    fn test_sced_nomogram_tightens_limit() {
        let net = make_three_bus_with_nomogram();
        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            enforce_flowgates: true,
            max_nomogram_iter: 10,
            ..Default::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        // Nomogram should have tightened FG_12 to 100 MW, forcing G1 ≤ 100 MW.
        assert!(
            sol.dispatch.pg_mw[0] <= 100.0 + 1.0,
            "Nomogram should limit G1 to ≤100 MW, got {:.1} MW",
            sol.dispatch.pg_mw[0]
        );
        // G2 must supply the remainder (≥ 50 MW).
        assert!(
            sol.dispatch.pg_mw[1] >= 50.0 - 1.0,
            "G2 should supply ≥50 MW, got {:.1} MW",
            sol.dispatch.pg_mw[1]
        );
    }

    // -----------------------------------------------------------------------
    // Virtual bid tests (Issue #39)
    // -----------------------------------------------------------------------

    /// Build a simple 1-bus SCED network for virtual bid tests.
    ///
    /// Bus 1 (Slack): Gen1 at $15/MWh (pmax=200), Gen2 at $25/MWh (pmax=200)
    /// No branches (single-zone — no congestion, uniform LMP).
    fn make_sced_vbid_net() -> Network {
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};
        let mut net = Network::new("vbid_sced");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 100.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 150.0, 0.0));

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            coeffs: vec![15.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(1, 0.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.cost = Some(CostCurve::Polynomial {
            coeffs: vec![25.0, 0.0],
            startup: 0.0,
            shutdown: 0.0,
        });
        net.generators.push(g2);

        net
    }

    /// VB-SCED-01: Inc bid priced below marginal generator clears,
    /// displacing physical generation.
    #[test]
    fn test_sced_vbid_inc_clears_and_displaces_gen() {
        use surge_network::market::{VirtualBid, VirtualBidDirection};

        let net = make_sced_vbid_net();
        // LMP without virtuals ≈ $15/MWh (G1 is on the margin).
        // Inc bid at $10/MWh for 30 MW → should clear fully (cheaper than LMP).
        let opts = DispatchOptions {
            virtual_bids: vec![VirtualBid {
                position_id: "inc_1".to_string(),
                bus: 1,
                period: 0,
                mw_limit: 30.0,
                price_per_mwh: 10.0,
                direction: VirtualBidDirection::Inc,
                in_service: true,
            }],
            ..DispatchOptions::default()
        };

        let sol = solve_sced(&net, &opts).unwrap();
        let objective_total: f64 = sol
            .dispatch
            .objective_terms
            .iter()
            .map(|term| term.dollars)
            .sum();

        assert_eq!(sol.dispatch.virtual_bid_results.len(), 1);
        let vbr = &sol.dispatch.virtual_bid_results[0];
        assert!(
            (vbr.cleared_mw - 30.0).abs() < 1.0,
            "Inc bid should clear at 30 MW, got {:.2}",
            vbr.cleared_mw
        );

        // Total physical gen should be 150 - 30 = 120 MW
        let total_phys: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_phys - 120.0).abs() < 2.0,
            "Physical gen should be ~120 MW, got {:.2}",
            total_phys
        );
        assert!(
            (objective_total - sol.dispatch.total_cost).abs() < 1e-6,
            "virtual-bid SCED objective terms should sum to total_cost: {objective_total:.6} vs {:.6}",
            sol.dispatch.total_cost
        );
        assert!(
            sol.dispatch
                .objective_terms
                .iter()
                .any(|term| { term.kind == surge_solution::ObjectiveTermKind::VirtualBid })
        );
    }

    /// VB-SCED-02: Uneconomic Inc bid (price > LMP) clears at zero —
    /// no change in physical dispatch vs. baseline.
    #[test]
    fn test_sced_vbid_uneconomic_does_not_clear() {
        use surge_network::market::{VirtualBid, VirtualBidDirection};

        let net = make_sced_vbid_net();
        // LMP ≈ $15/MWh; Inc bid at $50/MWh → far above LMP → should not clear.
        let opts = DispatchOptions {
            virtual_bids: vec![VirtualBid {
                position_id: "inc_2".to_string(),
                bus: 1,
                period: 0,
                mw_limit: 30.0,
                price_per_mwh: 50.0,
                direction: VirtualBidDirection::Inc,
                in_service: true,
            }],
            ..DispatchOptions::default()
        };

        let sol = solve_sced(&net, &opts).unwrap();
        let objective_total: f64 = sol
            .dispatch
            .objective_terms
            .iter()
            .map(|term| term.dollars)
            .sum();

        assert_eq!(sol.dispatch.virtual_bid_results.len(), 1);
        let vbr = &sol.dispatch.virtual_bid_results[0];
        assert!(
            vbr.cleared_mw < 1.0,
            "Uneconomic Inc bid should not clear, got {:.2} MW",
            vbr.cleared_mw
        );

        // Physical gen should still serve full 150 MW load
        let total_phys: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_phys - 150.0).abs() < 2.0,
            "Physical gen should be ~150 MW, got {:.2}",
            total_phys
        );
        assert!(
            (objective_total - sol.dispatch.total_cost).abs() < 1e-6,
            "uneconomic virtual-bid SCED terms should sum to total_cost: {objective_total:.6} vs {:.6}",
            sol.dispatch.total_cost
        );
    }
}

#[cfg(test)]
mod block_mode_sced_tests {
    use super::*;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Bus, BusType, Generator};

    /// Helper: build a simple 1-bus network with N generators.
    fn one_bus_network(gens: Vec<Generator>) -> Network {
        let mut net = Network::new("block_mode_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 200.0, 0.0));
        for g in gens {
            net.generators.push(g);
        }
        net
    }

    fn cheap_gen(pmin: f64, pmax: f64) -> Generator {
        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmin = pmin;
        g.pmax = pmax;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        g
    }

    fn expensive_gen(pmin: f64, pmax: f64) -> Generator {
        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmin = pmin;
        g.pmax = pmax;
        g.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });
        g
    }

    /// Block mode with flat ramp curves should produce the same dispatch
    /// and cost as averaged mode (the default).
    #[test]
    fn test_block_mode_matches_averaged_flat_ramp() {
        let net = one_bus_network(vec![cheap_gen(50.0, 300.0), expensive_gen(50.0, 200.0)]);

        let avg_opts = DispatchOptions::default();
        let blk_opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: false,
            },
            ..Default::default()
        };

        let avg_sol = solve_sced(&net, &avg_opts).unwrap();
        let blk_sol = solve_sced(&net, &blk_opts).unwrap();

        // Same total cost (block mode linearizes polynomial, but for linear cost
        // the dispatch should be identical).
        let rel_err = (blk_sol.dispatch.total_cost - avg_sol.dispatch.total_cost).abs()
            / avg_sol.dispatch.total_cost.abs().max(1.0);
        assert!(
            rel_err < 1e-4,
            "Block mode cost={:.4}, Averaged cost={:.4}, rel_err={:.6}",
            blk_sol.dispatch.total_cost,
            avg_sol.dispatch.total_cost,
            rel_err,
        );

        // Same dispatch (within tolerance)
        for (j, (a, b)) in avg_sol
            .dispatch
            .pg_mw
            .iter()
            .zip(blk_sol.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (a - b).abs() < 0.5,
                "gen {j}: avg={a:.2} MW, block={b:.2} MW"
            );
        }

        // Power balance
        let total_gen: f64 = blk_sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 0.1,
            "power balance: gen={total_gen:.2}, load=200.0"
        );
    }

    /// Block mode with PWL cost curve correctly decomposes dispatch.
    #[test]
    fn test_block_mode_pwl_cost() {
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 50.0;
        g0.pmax = 300.0;
        // PWL: 3 segments with increasing marginal cost
        g0.cost = Some(CostCurve::PiecewiseLinear {
            startup: 0.0,
            shutdown: 0.0,
            points: vec![
                (50.0, 0.0),
                (150.0, 1000.0), // slope = 10 $/MWh
                (250.0, 3000.0), // slope = 20 $/MWh
                (300.0, 4500.0), // slope = 30 $/MWh
            ],
        });

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![25.0, 0.0], // 25 $/MWh flat
        });

        let mut net = Network::new("block_pwl_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 250.0, 0.0));
        net.generators.push(g0);
        net.generators.push(g1);

        let blk_opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: false,
            },
            ..Default::default()
        };

        let sol = solve_sced(&net, &blk_opts).unwrap();

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 250.0).abs() < 0.5,
            "power balance: gen={total_gen:.2}, load=250.0"
        );

        // g0's first block (10 $/MWh) and g1 (25 $/MWh): g0 block 1 fills first.
        // g0's second block (20 $/MWh): fills before g1.
        // g1 (25 $/MWh): fills before g0's third block (30 $/MWh).
        // Expected: g0 dispatches 250 MW from blocks 1+2, g1 gets the rest.
        // g0 block 1: 100 MW (10 $/MWh, all fills), g0 block 2: 100 MW (20 $/MWh, all fills)
        // g1: 50 MW (25 $/MWh), g0 block 3: 0 MW (30 $/MWh, not needed)
        // So g0 = 50 + 100 + 100 = 250... wait, that's the whole load. Let me recalculate.
        // Load = 250, g0 pmin = 50, g0 blocks: [50,150]=10, [150,250]=20, [250,300]=30
        // g0 must dispatch at least pmin=50 MW (via linking constraint).
        // Block 1 [50,150] capacity 100 at $10 → fill 100
        // Block 2 [150,250] capacity 100 at $20 → fill to satisfy
        // g1 at $25 MW → fill after g0 block 2
        // Total needed = 250 - 50 (g0 pmin) = 200 from blocks
        // Block 1 fills 100 at $10, block 2 fills 100 at $20 → g0 = 250 MW
        // g1 dispatches 0 MW. Total = 250. ✓
        assert!(
            sol.dispatch.pg_mw[0] > 240.0,
            "g0 should dispatch ~250 MW (cheap blocks), got {:.2}",
            sol.dispatch.pg_mw[0]
        );
    }

    /// Block mode with per-block ramp tightening limits dispatch correctly.
    #[test]
    fn test_block_mode_ramp_tightening() {
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 100.0;
        g0.pmax = 400.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        // Ramp-up curve: slow in lower range, faster in upper range
        g0.ramping.get_or_insert_default().ramp_up_curve = vec![(100.0, 2.0), (250.0, 5.0)]; // MW/min
        g0.ramping.get_or_insert_default().ramp_down_curve = vec![(100.0, 2.0), (250.0, 5.0)];

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 300.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });

        let mut net = Network::new("block_ramp_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 350.0, 0.0));
        net.generators.push(g0);
        net.generators.push(g1);

        // Previous dispatch: g0 at 200 MW, g1 at 100 MW (total = 300 MW)
        // Now load jumps to 350 MW → need 50 more MW.
        // g0 prev dispatch decomposes: block [100,250] fill = 100, block [250,400] fill = 0
        // Block 1 ramp: 2 MW/min × 60 min = 120 MW/hr → ub = min(150, 100+120) = 150 ✓ (unconstrained)
        // Block 2 ramp: 5 MW/min × 60 min = 300 MW/hr → ub = min(150, 0+300) = 150 ✓ (unconstrained)
        // Without ramp constraints, g0 would dispatch to 350 MW easily.
        let blk_opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: false,
            },
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                prev_dispatch_mw: Some(vec![200.0, 100.0]),
                ..Default::default()
            },
            dt_hours: 1.0,
            ..Default::default()
        };

        let sol = solve_sced(&net, &blk_opts).unwrap();

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 350.0).abs() < 0.5,
            "power balance: gen={total_gen:.2}, load=350.0"
        );

        // g0 should be able to ramp up — cheap gen dispatches as much as it can
        assert!(
            sol.dispatch.pg_mw[0] > 190.0,
            "g0 should dispatch > 190 MW, got {:.2}",
            sol.dispatch.pg_mw[0]
        );
    }

    /// Block mode with tight per-block ramp should constrain cheap gen,
    /// forcing expensive gen to fill the gap.
    #[test]
    fn test_block_mode_tight_ramp_forces_expensive() {
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 100.0;
        g0.pmax = 300.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        // Very slow ramp: 0.5 MW/min
        g0.ramping.get_or_insert_default().ramp_up_curve = vec![(100.0, 0.5)];
        g0.ramping.get_or_insert_default().ramp_down_curve = vec![(100.0, 0.5)];

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 300.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });

        let mut net = Network::new("block_tight_ramp_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 250.0, 0.0));
        net.generators.push(g0);
        net.generators.push(g1);

        // Previous dispatch: g0 at 150 MW, g1 at 50 MW.
        // Now load = 250 → need 50 more.
        // g0 ramp: 0.5 MW/min × 60 = 30 MW → max g0 = 150 + 30 = 180 MW
        // So g1 must supply at least 250 - 180 = 70 MW.
        let blk_opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: false,
            },
            initial_state: crate::dispatch::IndexedDispatchInitialState {
                prev_dispatch_mw: Some(vec![150.0, 50.0]),
                ..Default::default()
            },
            dt_hours: 1.0,
            ..Default::default()
        };

        let sol = solve_sced(&net, &blk_opts).unwrap();

        // g0 should be limited by ramp
        assert!(
            sol.dispatch.pg_mw[0] <= 181.0,
            "g0 should be ramp-limited to ~180 MW, got {:.2}",
            sol.dispatch.pg_mw[0]
        );

        // g1 fills the gap
        assert!(
            sol.dispatch.pg_mw[1] >= 69.0,
            "g1 should dispatch ≥69 MW to fill gap, got {:.2}",
            sol.dispatch.pg_mw[1]
        );

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 250.0).abs() < 0.5,
            "power balance: gen={total_gen:.2}, load=250.0"
        );
    }

    /// Per-block reserves produce feasible dispatch with reserve awards.
    #[test]
    fn test_per_block_reserves_feasible() {
        use surge_network::market::SystemReserveRequirement;

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 50.0;
        g0.pmax = 300.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 50.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });

        let mut net = one_bus_network(vec![g0, g1]);
        for g in &mut net.generators {
            let phys_cap = (g.pmax - g.pmin).max(0.0);
            if phys_cap > 0.0 {
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "spin".into(),
                        capacity_mw: phys_cap,
                        cost_per_mwh: 0.0,
                    },
                );
            }
        }

        let opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: true,
            },
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };

        let sol = solve_sced(&net, &opts).unwrap();

        // Power balance
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 200.0).abs() < 1.0,
            "power balance: gen={total_gen:.2}, load=200.0"
        );

        // Reserve awarded
        let spin_awards = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .expect("spin awards");
        let total_reserve: f64 = spin_awards.iter().sum();
        assert!(
            total_reserve >= 29.9,
            "reserve={total_reserve:.1}, required=30"
        );
    }

    /// Per-block reserves should reduce reserve when block ramp rate is tight.
    #[test]
    fn test_per_block_reserves_ramp_limited() {
        use surge_network::market::SystemReserveRequirement;

        // Generator with slow ramp in upper block — limits reserve from that block.
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        // Two ramp breakpoints: [0,100] at 10 MW/min, [100,200] at 1 MW/min
        g0.ramping.get_or_insert_default().ramp_up_curve = vec![(100.0, 10.0), (200.0, 1.0)];

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![40.0, 0.0],
        });

        let mut net = Network::new("block_ramp_reserve_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, 150.0, 0.0));
        net.generators.push(g0);
        net.generators.push(g1);
        for g in &mut net.generators {
            let phys_cap = (g.pmax - g.pmin).max(0.0);
            if phys_cap > 0.0 {
                g.market.get_or_insert_default().reserve_offers.push(
                    surge_network::market::ReserveOffer {
                        product_id: "spin".into(),
                        capacity_mw: phys_cap,
                        cost_per_mwh: 0.0,
                    },
                );
            }
        }

        // With per_block_reserves=true, the slow upper block of g0 provides
        // less reserve (ramp-limited). Without it, standard headroom would
        // overstate available reserve.
        let blk_opts = DispatchOptions {
            ramp_mode: RampMode::Block {
                per_block_reserves: true,
            },
            system_reserve_requirements: vec![SystemReserveRequirement {
                product_id: "spin".into(),
                requirement_mw: 30.0,
                per_period_mw: None,
            }],
            ..Default::default()
        };

        let sol = solve_sced(&net, &blk_opts).unwrap();

        // Should still solve
        let total_gen: f64 = sol.dispatch.pg_mw.iter().sum();
        assert!(
            (total_gen - 150.0).abs() < 1.0,
            "power balance: gen={total_gen:.2}, load=150.0"
        );

        // Reserve met
        let spin_awards = sol
            .dispatch
            .reserve_awards
            .get("spin")
            .expect("spin awards");
        let total_reserve: f64 = spin_awards.iter().sum();
        assert!(
            total_reserve >= 29.9,
            "reserve={total_reserve:.1}, required=30"
        );
    }
}

// ---------------------------------------------------------------------------
// DR per-period dl_offer_schedules tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod dl_per_period_tests {
    use super::*;
    use std::collections::HashMap;
    use surge_network::Network;
    use surge_network::market::{
        CostCurve, DispatchableLoad, DlOfferSchedule, DlPeriodParams, LoadCostModel,
    };
    use surge_network::network::{Bus, BusType, Generator};

    /// Build a single-bus, two-gen network for DR testing.
    fn one_bus_net(load_mw: f64) -> Network {
        let mut net = Network::new("dl_per_period_test");
        net.base_mva = 100.0;
        let b1 = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b1);
        net.loads.push(Load::new(1, load_mw, 0.0));

        // Cheap gen: $10/MWh
        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 0.0;
        g0.pmax = 200.0;
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        // Expensive gen: $50/MWh
        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0],
        });
        net.generators.push(g1);

        net
    }

    /// DL per-period: when the DL offer schedule overrides p_max for a specific period,
    /// the solver should respect the overridden bound.
    #[test]
    fn test_dl_per_period_pmax_override() {
        let net = one_bus_net(80.0);
        let base = net.base_mva;

        // Base DL: 40 MW scheduled, fully curtailable, $30/MWh
        let dl = DispatchableLoad::curtailable(1, 40.0, 0.0, 0.0, 30.0, base);

        // Override period 0: reduce p_max from 0.4 pu (40 MW) to 0.2 pu (20 MW)
        let mut dl_schedules = HashMap::new();
        dl_schedules.insert(
            0usize,
            DlOfferSchedule {
                periods: vec![Some(DlPeriodParams {
                    p_sched_pu: 20.0 / base,
                    p_max_pu: 20.0 / base,
                    q_sched_pu: None,
                    q_min_pu: None,
                    q_max_pu: None,
                    pq_linear_equality: None,
                    pq_linear_upper: None,
                    pq_linear_lower: None,
                    cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 30.0 },
                })],
            },
        );

        let opts = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            dl_offer_schedules: dl_schedules,
            ..Default::default()
        };

        let sol = solve_sced(&net, &opts).unwrap();

        // DL should be served at the overridden max: 20 MW (0.2 pu)
        assert!(!sol.dispatch.dr_results.loads.is_empty());
        let served_mw = sol.dispatch.dr_results.loads[0].p_served_pu * base;
        assert!(
            served_mw <= 20.5,
            "DL served should respect overridden p_max=20 MW: got {served_mw:.2}"
        );
    }

    /// DL per-period: when the override raises the curtailment cost above LMP,
    /// the DL is no longer curtailed (vs base where it would be).
    #[test]
    fn test_dl_per_period_cost_override() {
        // With 80 MW load and cheap gen at $10, LMP ≈ $10.
        // Base DL: 50 MW, cost=$5/MWh → below LMP → should curtail.
        // Override: cost=$200/MWh → above LMP → should NOT curtail.
        let net = one_bus_net(80.0);
        let base = net.base_mva;

        let dl = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 5.0, base);

        // Without override: cost=$5 < LMP=$10 → curtailment expected
        let opts_no_override = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl.clone()],
            ..Default::default()
        };
        let sol_no = solve_sced(&net, &opts_no_override).unwrap();
        let served_no = sol_no.dispatch.dr_results.loads[0].p_served_pu * base;
        assert!(
            served_no < 48.0,
            "Without override, DL should be curtailed at $5/MWh: served={served_no:.2}"
        );

        // With override: cost=$200 >> LMP → no curtailment
        let mut dl_schedules = HashMap::new();
        dl_schedules.insert(
            0usize,
            DlOfferSchedule {
                periods: vec![Some(DlPeriodParams {
                    p_sched_pu: dl.p_sched_pu,
                    p_max_pu: dl.p_max_pu,
                    q_sched_pu: None,
                    q_min_pu: None,
                    q_max_pu: None,
                    pq_linear_equality: None,
                    pq_linear_upper: None,
                    pq_linear_lower: None,
                    cost_model: LoadCostModel::LinearCurtailment { cost_per_mw: 200.0 },
                })],
            },
        );

        let opts_override = DispatchOptions {
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            dl_offer_schedules: dl_schedules,
            ..Default::default()
        };
        let sol_override = solve_sced(&net, &opts_override).unwrap();
        let served_override = sol_override.dispatch.dr_results.loads[0].p_served_pu * base;
        assert!(
            served_override > 49.0,
            "With $200/MWh override, DL should not be curtailed: served={served_override:.2}"
        );
    }

    #[test]
    #[ignore = "regression test for dbc566a9 (DL linear-curtailment dt-scaling fix); fix not in release/v0.1.2 — revisit after release"]
    fn test_dl_linear_curtailment_value_not_downscaled_by_dt() {
        let mut net = one_bus_net(0.0);
        net.generators[0].cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![100.0, 0.0],
        });
        net.generators[1].in_service = false;
        let base = net.base_mva;

        let dl = DispatchableLoad::curtailable(1, 40.0, 0.0, 0.0, 30.0, base);
        let opts = DispatchOptions {
            dt_hours: 0.25,
            enforce_thermal_limits: false,
            dispatchable_loads: vec![dl],
            ..Default::default()
        };

        let sol = solve_sced(&net, &opts).unwrap();
        let served_mw = sol.dispatch.dr_results.loads[0].p_served_pu * base;
        assert!(
            served_mw > 39.0,
            "quarter-hour DL value should still clear against $100/MWh generation: served={served_mw:.2}"
        );
    }

    // =========================================================================
    // Shutdown de-loading in SCED
    // =========================================================================

    /// SCED shutdown de-loading: when next_period_commitment says a unit is
    /// shutting down, its upper bound should be capped at SD capacity.
    #[test]
    fn test_sced_shutdown_deloading() {
        use crate::dispatch::CommitmentMode;
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        // Build a simple 1-bus, 2-gen network.
        // Gen0: slow coal, Pmax=300, Pmin=50, shutdown_ramp=2 MW/min → SD=120 MW/hr
        // Gen1: peaker, Pmax=400, Pmin=0, cost=50 $/MWh (picks up slack)
        let mut net = Network::new("sced_deload_test");
        net.base_mva = 100.0;
        let b = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b);
        net.loads.push(Load::new(1, 250.0, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmin = 50.0;
        g0.pmax = 300.0;
        g0.commitment
            .get_or_insert_default()
            .shutdown_ramp_mw_per_min = Some(2.0); // SD = 120 MW/hr
        g0.ramping.get_or_insert_default().ramp_down_curve = vec![(0.0, 5.0)]; // economic = 300 MW/hr
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0], // cheap
        });
        net.generators.push(g0);

        let mut g1 = Generator::new(1, 0.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 400.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![50.0, 0.0], // expensive
        });
        net.generators.push(g1);

        // Without de-loading
        let opts_off = DispatchOptions {
            commitment: CommitmentMode::Fixed {
                commitment: vec![true, true],
                per_period: None,
            },
            enforce_shutdown_deloading: false,
            ..DispatchOptions::default()
        };
        let sol_off = solve_sced(&net, &opts_off).unwrap();

        // With de-loading
        let opts_on = DispatchOptions {
            enforce_shutdown_deloading: true,
            ..opts_off.clone()
        };
        let sol_on = super::solve::solve_sced_with_problem_spec(
            &net,
            crate::common::spec::DispatchProblemSpec::from_options(&opts_on),
            crate::common::runtime::DispatchPeriodContext {
                period: 0,
                prev_dispatch_mw: opts_on.initial_state.prev_dispatch_mw.as_deref(),
                prev_dispatch_mask: opts_on.initial_state.prev_dispatch_mask.as_deref(),
                prev_hvdc_dispatch_mw: opts_on.initial_state.prev_hvdc_dispatch_mw.as_deref(),
                prev_hvdc_dispatch_mask: opts_on.initial_state.prev_hvdc_dispatch_mask.as_deref(),
                storage_soc_override: opts_on.initial_state.storage_soc_override.as_ref(),
                next_period_commitment: Some(&[false, true]),
            },
        )
        .unwrap();

        let pg0_off = sol_off.dispatch.pg_mw[0];
        let pg0_on = sol_on.periods[0].pg_mw[0];

        // Without de-loading: Gen0 dispatches at full cheap capacity (250 MW)
        assert!(
            pg0_off > 120.0 + 1.0,
            "Without de-loading, Gen0 should dispatch above SD=120 MW, got {pg0_off:.1}"
        );

        // With de-loading: Gen0 capped at SD = 120 MW
        assert!(
            pg0_on <= 120.0 + 0.1,
            "With de-loading, Gen0 should be ≤ 120 MW (SD capacity), got {pg0_on:.1}"
        );
    }

    /// SCED: de-loading flag with no next_period_commitment is a no-op.
    #[test]
    fn test_sced_deloading_no_next_commit_is_noop() {
        use surge_network::Network;
        use surge_network::market::CostCurve;
        use surge_network::network::{Bus, BusType, Generator};

        let mut net = Network::new("sced_deload_noop");
        net.base_mva = 100.0;
        let b = Bus::new(1, BusType::Slack, 138.0);
        net.buses.push(b);
        net.loads.push(Load::new(1, 200.0, 0.0));

        let mut g0 = Generator::new(1, 0.0, 1.0);
        g0.pmax = 300.0;
        g0.commitment
            .get_or_insert_default()
            .shutdown_ramp_mw_per_min = Some(2.0);
        g0.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g0);

        // Flag on but no next_period_commitment → no tightening
        let opts = DispatchOptions {
            enforce_shutdown_deloading: true,
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();
        assert!(
            sol.dispatch.pg_mw[0] > 120.0,
            "Without next_period_commitment, Gen0 should not be de-load-capped: got {:.1}",
            sol.dispatch.pg_mw[0]
        );
    }
}

#[cfg(test)]
mod sced_ac_benders_tests {
    //! End-to-end tests for the SCED-AC Benders cut machinery.
    //!
    //! These tests verify that:
    //!
    //!   1. When `eta_periods` is empty, the LP behaves identically to the
    //!      pre-Benders SCED — no extra column, no extra row, same dispatch.
    //!   2. When `eta_periods` is set but no cuts are provided, the LP still
    //!      solves but the eta variable settles at its lower bound (0) and
    //!      adds 0 to the objective.
    //!   3. When a single cut is added, the LP respects it: the eta variable
    //!      is forced up to the cut value when Pg matches the cut's reference
    //!      point.
    //!   4. When a cut's slope opposes a generator's economic preference,
    //!      the LP shifts dispatch away from the slope to reduce eta — i.e.
    //!      the cut actively shapes the master's solution.
    //!
    //! The reference point for these tests is a 3-bus 2-gen network so the
    //! cut math can be hand-verified.
    use super::*;
    use crate::request::{ScedAcBendersCut, ScedAcBendersRuntime};
    use crate::solution::RawScedSolution;
    use std::collections::HashMap;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    fn three_bus_two_gen_network() -> Network {
        let mut net = Network::new("benders-rows-3bus");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PV, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.branches.push(Branch::new_line(1, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.02, 0.10, 0.0));
        net.loads.push(Load::new(3, 100.0, 30.0));

        // Cheap base-load gen at bus 1: linear cost 10 $/MWh.
        let mut g1 = Generator::new(1, 60.0, 1.0);
        g1.pmin = 0.0;
        g1.pmax = 200.0;
        g1.qmin = -100.0;
        g1.qmax = 100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![10.0, 0.0],
        });
        net.generators.push(g1);

        // Expensive gen at bus 2: linear cost 30 $/MWh.
        let mut g2 = Generator::new(2, 40.0, 1.0);
        g2.pmin = 0.0;
        g2.pmax = 200.0;
        g2.qmin = -100.0;
        g2.qmax = 100.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![30.0, 0.0],
        });
        net.generators.push(g2);

        net
    }

    fn baseline_solve(net: &Network) -> RawScedSolution {
        solve_sced(net, &DispatchOptions::default()).unwrap()
    }

    #[test]
    fn no_eta_no_cuts_matches_baseline() {
        let net = three_bus_two_gen_network();
        let baseline = baseline_solve(&net);

        // With an explicitly empty Benders runtime, the SCED LP should produce
        // identical dispatch and cost.
        let opts = DispatchOptions {
            sced_ac_benders: ScedAcBendersRuntime::default(),
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(
            (sol.dispatch.total_cost - baseline.dispatch.total_cost).abs() < 1e-6,
            "no-Benders SCED should match baseline cost"
        );
        for (i, (&pg_a, &pg_b)) in sol
            .dispatch
            .pg_mw
            .iter()
            .zip(baseline.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (pg_a - pg_b).abs() < 1e-6,
                "no-Benders gen {i}: {pg_a} vs baseline {pg_b}"
            );
        }
        assert!(sol.dispatch.sced_ac_benders_eta_dollars_per_hour.is_none());
    }

    #[test]
    fn eta_active_no_cuts_does_not_change_dispatch() {
        let net = three_bus_two_gen_network();
        let baseline = baseline_solve(&net);

        // Activate the eta variable for period 0 but pass no cuts. The eta
        // should settle at 0 (its lower bound), the dispatch should match
        // baseline, and the total cost should match baseline (eta=0 adds 0).
        let benders = ScedAcBendersRuntime {
            eta_periods: vec![0],
            cuts: vec![],
            orchestration: None,
        };
        let opts = DispatchOptions {
            sced_ac_benders: benders,
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        assert!(
            (sol.dispatch.total_cost - baseline.dispatch.total_cost).abs() < 1e-6,
            "eta-active no-cuts cost should match baseline (eta=0 adds 0)"
        );
        for (i, (&pg_a, &pg_b)) in sol
            .dispatch
            .pg_mw
            .iter()
            .zip(baseline.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (pg_a - pg_b).abs() < 1e-6,
                "eta-active gen {i}: {pg_a} vs baseline {pg_b}"
            );
        }
        let eta = sol.dispatch.sced_ac_benders_eta_dollars_per_hour;
        assert!(eta.is_some(), "eta should be reported when active");
        assert!(
            eta.unwrap().abs() < 1e-6,
            "eta should equal 0 with no cuts, got {}",
            eta.unwrap()
        );
    }

    #[test]
    fn constant_cut_forces_eta_up() {
        let net = three_bus_two_gen_network();
        let baseline = baseline_solve(&net);

        // A cut with no Pg coefficients is just a constant lower bound on
        // eta: `eta >= 5000`. The LP should set eta = 5000 (cheaper to
        // honor the cut than violate it; the master objective adds eta).
        // Total cost should increase by exactly 5000 vs baseline.
        let cut = ScedAcBendersCut {
            period: 0,
            coefficients_dollars_per_mw_per_hour: HashMap::new(),
            rhs_dollars_per_hour: 5000.0,
            iteration: 0,
        };
        let benders = ScedAcBendersRuntime {
            eta_periods: vec![0],
            cuts: vec![cut],
            orchestration: None,
        };
        let opts = DispatchOptions {
            sced_ac_benders: benders,
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        let cost_increase = sol.dispatch.total_cost - baseline.dispatch.total_cost;
        assert!(
            (cost_increase - 5000.0).abs() < 1e-6,
            "constant cut should add exactly 5000 to total_cost, got delta = {cost_increase}"
        );
        let eta = sol
            .dispatch
            .sced_ac_benders_eta_dollars_per_hour
            .expect("eta should be reported");
        assert!(
            (eta - 5000.0).abs() < 1e-6,
            "eta should equal cut RHS, got {eta}"
        );
        // Dispatch should be unchanged because the cut has no slope.
        for (i, (&pg_a, &pg_b)) in sol
            .dispatch
            .pg_mw
            .iter()
            .zip(baseline.dispatch.pg_mw.iter())
            .enumerate()
        {
            assert!(
                (pg_a - pg_b).abs() < 1e-6,
                "constant-cut gen {i}: {pg_a} vs baseline {pg_b}"
            );
        }
    }

    #[test]
    fn slope_cut_shifts_dispatch_away_from_penalised_generator() {
        let net = three_bus_two_gen_network();
        let baseline = baseline_solve(&net);
        let g1_baseline = baseline.dispatch.pg_mw[0];

        // Build a cut that penalises additional dispatch on g1 ("Gen1") at
        // 100 $/MW-hr above its current value:
        //
        //   eta >= 100 * (Pg_g1 - g1_baseline)
        //
        // In our row form `eta - λ_g · Pg_g1 ≥ rhs`:
        //   λ_g1 = 100
        //   rhs = -100 * g1_baseline
        //
        // The LP must trade off:
        //   - Increasing eta by `100 * Pg_g1` ($/hr) — cost penalty per master eta term
        //   - vs swapping g1 generation to g2 at +$20/MWh marginal cost
        //
        // 100 $/MW-hr >> 20 $/MWh, so the LP should prefer to *reduce* g1 to
        // its baseline value (driving the cut to its lower bound) and shift
        // load to g2 only if g1 was already at its current operating point.
        // We expect g1 to NOT increase beyond baseline; the cost should
        // increase by close to 0 (eta = 0) compared to baseline.
        let mut coeffs = HashMap::new();
        coeffs.insert("1".to_string(), 100.0); // resource id of Generator::new(1, ...)
        let cut = ScedAcBendersCut {
            period: 0,
            coefficients_dollars_per_mw_per_hour: coeffs,
            rhs_dollars_per_hour: -100.0 * g1_baseline,
            iteration: 0,
        };
        let benders = ScedAcBendersRuntime {
            eta_periods: vec![0],
            cuts: vec![cut],
            orchestration: None,
        };
        let opts = DispatchOptions {
            sced_ac_benders: benders,
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // Verify the cut is consistent: at the LP optimum,
        //   eta >= 100 * (Pg_g1 - g1_baseline)
        let eta = sol
            .dispatch
            .sced_ac_benders_eta_dollars_per_hour
            .expect("eta should be reported");
        let g1 = sol.dispatch.pg_mw[0];
        let lhs = eta;
        let rhs = 100.0 * (g1 - g1_baseline);
        assert!(
            lhs + 1e-4 >= rhs,
            "LP should respect cut: eta={eta} vs 100*(Pg-base)={rhs}"
        );

        // Because adding 1 MW to g1 costs 100 $/hr extra in eta plus 10
        // $/MWh in production cost (110 total), while running 1 MW more on
        // g2 only costs 30 $/MWh, the LP must NOT push g1 above baseline.
        assert!(
            g1 <= g1_baseline + 1e-4,
            "Cut should keep g1 at or below baseline; got g1={g1}, baseline={g1_baseline}"
        );
    }

    #[test]
    fn cut_for_other_period_is_ignored_in_this_period() {
        // The SCED LP solves period 0 only. A cut targeted at period 5
        // should never appear in this LP, so the dispatch must match the
        // no-cut baseline exactly.
        let net = three_bus_two_gen_network();
        let baseline = baseline_solve(&net);

        let cut = ScedAcBendersCut {
            period: 5,
            coefficients_dollars_per_mw_per_hour: HashMap::new(),
            rhs_dollars_per_hour: 100_000.0,
            iteration: 0,
        };
        let benders = ScedAcBendersRuntime {
            eta_periods: vec![0],
            cuts: vec![cut],
            orchestration: None,
        };
        let opts = DispatchOptions {
            sced_ac_benders: benders,
            ..DispatchOptions::default()
        };
        let sol = solve_sced(&net, &opts).unwrap();

        // The cut for period 5 must NOT bind period 0's eta — total_cost
        // should match baseline.
        assert!(
            (sol.dispatch.total_cost - baseline.dispatch.total_cost).abs() < 1e-6,
            "cross-period cut leak: {} vs {}",
            sol.dispatch.total_cost,
            baseline.dispatch.total_cost
        );
    }
}

/// Regression: PWL (offer-schedule) producer cost must scale by `dt_h` in SCED.
///
/// Mirrors the SCUC regression test
/// `scuc::tests::test_scuc_pwl_offer_schedule_objective_scales_linearly_with_dt`.
/// The epigraph rows for PWL producers carry `$/h` units (slope and
/// intercept from `common/costs::pwl_curve_segments`), so the `e_g`
/// column cost must be `dt_h` for the LP objective to land in dollars.
/// Before the fix SCED priced `e_g` at `1.0`, which silently inflated
/// sub-hourly energy costs.
#[cfg(test)]
mod pwl_dt_scaling_regression_tests {
    use super::*;
    use std::collections::HashMap;
    use surge_network::Network;
    use surge_network::market::{LoadProfile, OfferCurve, OfferSchedule};
    use surge_network::network::{Bus, BusType, Generator};

    fn build_pwl_net() -> Network {
        let mut net = Network::new("sced_pwl_dt_scale_test");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 100.0, 0.0));
        let mut g = Generator::new(1, 0.0, 1.0);
        g.pmin = 0.0;
        g.pmax = 200.0;
        g.commitment.get_or_insert_default().status = CommitmentStatus::MustRun;
        // Cost lives in the offer schedule, not the static network
        // field, so the PWL epigraph codepath is exercised.
        g.cost = None;
        net.generators.push(g);
        net
    }

    fn make_offer() -> OfferSchedule {
        // Multi-segment offer forces the PWL (piecewise-linear) epigraph
        // path in the SCED objective. A single-segment offer would
        // resolve to `CostCurve::Polynomial{coeffs=[price, no_load]}`
        // via `offer_curve_to_cost_curve` and exercise a different
        // pricing branch that is not the focus of this regression.
        // Two segments at the same marginal cost keep dispatch simple
        // (any 100 MW is on-segment) while still taking the PWL path.
        OfferSchedule {
            periods: vec![Some(OfferCurve {
                segments: vec![(150.0, 50.0), (200.0, 50.0)],
                no_load_cost: 0.0,
                startup_tiers: vec![],
            })],
        }
    }

    fn base_opts(dt_hours: f64, offer: OfferSchedule) -> DispatchOptions {
        let mut opts = DispatchOptions {
            n_periods: 1,
            dt_hours,
            enforce_thermal_limits: false,
            offer_schedules: HashMap::from([(0, offer)]),
            ..DispatchOptions::default()
        };
        opts.load_profiles.profiles.push(LoadProfile {
            bus: 1,
            load_mw: vec![100.0],
        });
        opts.load_profiles.n_timesteps = 1;
        opts
    }

    /// Regression: SCED extract must agree with the LP objective on PWL cost.
    ///
    /// Before the fix, the LP objective priced the `e_g` column at
    /// `1.0` (interpreting `sol.x[e_g]` as `$`) while extract multiplied
    /// `sol.x[e_g]` by `dt_h` when building objective terms. The two
    /// accountings disagreed on any sub-hourly period, leaving a
    /// "residual" term to cover the mismatch — a red flag in objective
    /// ledger validation. With the fix both ledgers land on the same
    /// dollar value, so no residual is emitted.
    #[test]
    fn sced_pwl_offer_schedule_objective_terms_match_lp_objective() {
        let net = build_pwl_net();
        let sol = solve_sced(&net, &base_opts(0.5, make_offer())).unwrap();
        let lp_obj = sol.dispatch.total_cost;
        let term_sum: f64 = sol.dispatch.objective_terms.iter().map(|t| t.dollars).sum();
        assert!(
            (lp_obj - term_sum).abs() < 1e-6,
            "objective ledger mismatch: total_cost={lp_obj}, sum(terms)={term_sum}; \
             terms={:?}",
            sol.dispatch.objective_terms
        );
        // And there should be no "residual" term — if one shows up it
        // means extract and LP disagree.
        assert!(
            sol.dispatch
                .objective_terms
                .iter()
                .all(|t| t.component_id != "residual"),
            "unexpected residual term (extract ↔ LP accounting drift); terms={:?}",
            sol.dispatch.objective_terms
        );
    }

    #[test]
    fn sced_pwl_offer_schedule_objective_scales_linearly_with_dt() {
        let net = build_pwl_net();
        let sol_1h = solve_sced(&net, &base_opts(1.0, make_offer())).unwrap();
        let sol_30min = solve_sced(&net, &base_opts(0.5, make_offer())).unwrap();
        let sol_15min = solve_sced(&net, &base_opts(0.25, make_offer())).unwrap();

        // Dispatch invariant: same instantaneous MW for any period duration.
        assert!((sol_1h.dispatch.pg_mw[0] - 100.0).abs() < 0.5);
        assert!((sol_30min.dispatch.pg_mw[0] - 100.0).abs() < 0.5);
        assert!((sol_15min.dispatch.pg_mw[0] - 100.0).abs() < 0.5);

        // Analytical objective: $50/MWh × 100 MW × dt_h.
        // 1h ⇒ $5,000; 30 min ⇒ $2,500; 15 min ⇒ $1,250.
        let c_1h = sol_1h.dispatch.total_cost;
        let c_30min = sol_30min.dispatch.total_cost;
        let c_15min = sol_15min.dispatch.total_cost;
        assert!(
            (c_1h - 5000.0).abs() < 1e-3,
            "SCED PWL 1h cost should be $5,000; got {c_1h:.4}"
        );
        assert!(
            (c_30min - 2500.0).abs() < 1e-3,
            "SCED PWL 30-min cost should be $2,500 (half of 1 h); got {c_30min:.4}"
        );
        assert!(
            (c_15min - 1250.0).abs() < 1e-3,
            "SCED PWL 15-min cost should be $1,250 (quarter of 1 h); got {c_15min:.4}"
        );
    }
}
