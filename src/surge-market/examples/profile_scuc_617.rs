// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Profile target: reproduce the SCUC-only portion of the 617-bus
//! D1/scenario_002 solve from Rust, with no Python in the loop, so
//! samply/perf/flamegraph can attribute cleanly.
//!
//! Usage:
//!   cargo run --profile release-dev --example profile_scuc_617 -p surge-market
//!   samply record -- target/release-dev/examples/profile_scuc_617

use std::path::Path;
use std::time::Instant;

use surge_io::go_c3::{
    GoC3Policy, apply_hvdc_reactive_terminals, apply_reserves, apply_voltage_regulation,
    enrich_network,
};
use surge_io::go_c3::{load_problem, to_network_with_policy};
use surge_market::canonical_workflow::CanonicalWorkflowOptions;
use surge_market::go_c3::build_canonical_workflow;
use surge_market::solve_market_workflow_until;

fn main() {
    let problem_path =
        Path::new("target/benchmarks/go-c3/datasets/event4_617/D1/C3E4N00617D1/scenario_002.json");

    let t0 = Instant::now();
    let problem = load_problem(problem_path).expect("load_problem");
    eprintln!("load_problem: {:.3}s", t0.elapsed().as_secs_f64());

    // Default policy is fine for profiling (MIP gap lives further down
    // the stack, not on GoC3Policy). We care about build/extract/drop
    // paths, not the solver's branch-and-bound cost.
    let policy = GoC3Policy::default();

    let t = Instant::now();
    let (mut network, mut context) =
        to_network_with_policy(&problem, &policy).expect("to_network_with_policy");
    enrich_network(&mut network, &mut context, &problem, &policy).expect("enrich_network");
    apply_reserves(&mut network, &mut context, &problem).expect("apply_reserves");
    apply_hvdc_reactive_terminals(&mut network, &mut context, &problem, &policy)
        .expect("apply_hvdc_reactive_terminals");
    apply_voltage_regulation(&mut network, &mut context, &problem, &policy)
        .expect("apply_voltage_regulation");
    eprintln!("network_build: {:.3}s", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let workflow = build_canonical_workflow(
        &problem,
        &mut context,
        &policy,
        &mut network,
        CanonicalWorkflowOptions::default(),
    )
    .expect("build_canonical_workflow");
    eprintln!(
        "build_canonical_workflow: {:.3}s",
        t.elapsed().as_secs_f64()
    );

    // Stop after SCUC so we only profile the path the user is drilling into.
    let t = Instant::now();
    let result = solve_market_workflow_until(&workflow, "scuc", None)
        .expect("solve_market_workflow_until(scuc)");
    eprintln!("solve_scuc: {:.3}s", t.elapsed().as_secs_f64());

    if let Some(stage) = result.stages.first() {
        let diag = stage.solution.diagnostics();
        eprintln!(
            "scuc diagnostics: iterations={} solve_time_secs={:.3}",
            diag.iterations, diag.solve_time_secs,
        );
        if let Some(pt) = &diag.phase_timings {
            eprintln!(
                "phase_timings:\n  \
                 prepare_request={:.3}  problem_spec={:.3}  network_snapshot={:.3}  \
                 build_session={:.3}  build_model_plan={:.3}  build_problem_plan={:.3}  \
                 build_problem={:.3}  solve_problem={:.3}  pricing={:.3}  \
                 extract_solution={:.3}  attach_public_catalogs={:.3}  \
                 attach_keyed_period_views={:.3}  emit_keyed={:.3}  \
                 solve_prepared_raw_total={:.3}",
                pt.prepare_request_secs,
                pt.problem_spec_secs,
                pt.network_snapshot_secs,
                pt.build_session_secs,
                pt.build_model_plan_secs,
                pt.build_problem_plan_secs,
                pt.build_problem_secs,
                pt.solve_problem_secs,
                pt.pricing_secs,
                pt.extract_solution_secs,
                pt.attach_public_catalogs_secs,
                pt.attach_keyed_period_views_secs,
                pt.emit_keyed_secs,
                pt.solve_prepared_raw_total_secs,
            );
        }
    }
}
