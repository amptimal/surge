// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Surge CLI — Command-line power flow solver.
//!
//! Usage:
//!   surge-solve `<case-file>` [--method acpf|fdpf|dcpf] [--tolerance 1e-8] [--max-iter 50]

mod cli;
mod output;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::Serialize;

use cli::*;
use output::*;

fn format_optional_iterations(iterations: Option<u32>) -> String {
    iterations
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result_kind", content = "result", rename_all = "snake_case")]
enum SolvedStateResult {
    PowerFlow(surge_solution::PfSolution),
    Opf(surge_solution::OpfSolution),
    DcOpf(surge_opf::DcOpfResult),
    Scopf(surge_opf::ScopfResult),
    Hvdc(surge_hvdc::HvdcSolution),
}

#[derive(Debug, Clone, Serialize)]
struct CliTermination {
    kind: &'static str,
    converged: bool,
    optimality_proven: bool,
    hard_feasible: Option<bool>,
    discrete_feasible: Option<bool>,
}

#[derive(Debug, Serialize)]
struct CliResultEnvelope<'a, T: Serialize> {
    termination: CliTermination,
    result: &'a T,
}

fn dc_opf_json_envelope(
    result: &surge_opf::DcOpfResult,
) -> CliResultEnvelope<'_, surge_opf::DcOpfResult> {
    CliResultEnvelope {
        termination: CliTermination {
            kind: "lp_optimal",
            converged: true,
            optimality_proven: true,
            hard_feasible: Some(result.is_feasible),
            discrete_feasible: None,
        },
        result,
    }
}

fn ac_opf_json_envelope(
    result: &surge_solution::OpfSolution,
) -> CliResultEnvelope<'_, surge_solution::OpfSolution> {
    CliResultEnvelope {
        termination: CliTermination {
            kind: "local_nlp_solution",
            converged: result.power_flow.status == surge_solution::SolveStatus::Converged,
            optimality_proven: false,
            hard_feasible: None,
            discrete_feasible: result.devices.discrete_feasible,
        },
        result,
    }
}

fn method_supports_angle_reference(method: CliMethod) -> bool {
    matches!(
        method,
        CliMethod::Dcpf | CliMethod::Acpf | CliMethod::AcpfWarm
    )
}

fn is_solved_state_export_path(path: &std::path::Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".json") || lower.ends_with(".json.zst")
}

#[derive(Debug, Clone, Serialize)]
struct SolvedStateArtifact {
    artifact_version: u32,
    method: String,
    network: surge_network::Network,
    #[serde(flatten)]
    result: SolvedStateResult,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Build log filter: RUST_LOG env overrides CLI flags.
    let env_filter = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::EnvFilter::from_default_env()
    } else if cli.quiet {
        tracing_subscriber::EnvFilter::new("error")
    } else {
        match cli.verbose {
            0 => tracing_subscriber::EnvFilter::new("warn"),
            1 => tracing_subscriber::EnvFilter::new("info"),
            2 => tracing_subscriber::EnvFilter::new("debug"),
            _ => tracing_subscriber::EnvFilter::new("trace"),
        }
    };

    match cli.log_format {
        TextOrJson::Json => {
            tracing_subscriber::fmt()
                .json()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(true)
                .init();
        }
        TextOrJson::Text => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(false)
                .init();
        }
    }

    let uses_lp_solver = method_uses_lp_solver(&cli);
    let uses_nlp_solver = method_uses_nlp_solver(&cli);

    // Resolve method-specific solver backends from --solver.
    if cli.solver != SolverBackend::Auto && !uses_lp_solver && !uses_nlp_solver {
        eprintln!(
            "Warning: --solver is only used for OPF/dispatch methods \
             (dc-opf, ac-opf, socp-opf, scopf, orpd, ots). \
             Ignoring --solver {} for --method {}.",
            cli_value_name(&cli.solver),
            cli_value_name(&cli.method)
        );
    }
    if cli.solver == SolverBackend::Highs && uses_nlp_solver {
        anyhow::bail!(
            "--solver highs is LP-only. For NLP methods use --solver ipopt, --solver copt, --solver gurobi, or --solver default."
        );
    }
    if cli.solver == SolverBackend::Ipopt && !uses_nlp_solver {
        anyhow::bail!("--solver ipopt is only valid for NLP methods (ac-opf, orpd, AC-SCOPF)");
    }
    let lp_solver = if uses_lp_solver && cli.solver != SolverBackend::Ipopt {
        make_lp_solver(cli.solver)?
    } else {
        None
    };

    // Validate --tolerance before use.
    // NaN/infinity or non-positive tolerances produce nonsensical solver behavior.
    if !cli.tolerance.is_finite() || cli.tolerance <= 0.0 {
        anyhow::bail!(
            "--tolerance must be a positive finite number, got {}",
            cli.tolerance
        );
    }
    if cli.tolerance < 1e-15 {
        anyhow::bail!(
            "--tolerance {} is unrealistically small (minimum 1e-15). \
             Typical values: 1e-8 (default), 1e-6 (loose), 1e-10 (tight).",
            cli.tolerance
        );
    }

    // Validate --max-iter before use.
    if cli.max_iter == 0 {
        anyhow::bail!("--max-iter must be at least 1");
    }
    if cli.max_iter > 10_000 {
        anyhow::bail!(
            "--max-iter {} is unreasonably large (maximum 10,000). \
             Typical values: 50 (tight), 100 (default for OPF), 500 (NR default).",
            cli.max_iter
        );
    }
    // Parse case file (auto-detect format from extension)
    let case_file = &cli.case_file;
    let network = surge_io::load(case_file)
        .with_context(|| format!("failed to parse case file: {}", case_file.display()))?;

    // Validate network structure before solving.
    if let Err(e) = network.validate() {
        anyhow::bail!("Network validation failed: {}", e);
    }

    if !method_supports_angle_reference(cli.method)
        && cli.angle_reference != CliAngleReference::PreserveInitial
    {
        anyhow::bail!("--angle-reference is only supported for dcpf, acpf, and acpf-warm");
    }

    if let Some(export_path) = cli.export.as_ref() {
        if cli.export_format.is_some() {
            anyhow::bail!("--export-format only applies to --convert");
        }
        if !is_solved_state_export_path(export_path) {
            anyhow::bail!(
                "--export writes a solved-state JSON artifact; use a .json or .json.zst path"
            );
        }
    }

    // Suppress human-readable summary text when --quiet is set. JSON output is
    // handled separately so successful JSON runs still emit machine-readable stdout.
    let json_mode = cli.output == TextOrJson::Json;
    let suppress_stdout = cli.quiet;
    if !suppress_stdout && !json_mode {
        println!(
            "Network: {} ({} buses, {} branches, {} generators)",
            network.name,
            network.n_buses(),
            network.n_branches(),
            network.generators.len()
        );
        println!(
            "Total generation: {:.1} MW, Total load: {:.1} MW",
            network.total_generation_mw(),
            network.total_load_mw()
        );
    }
    let text_detail = resolve_text_detail(cli.detail, &network);

    // Handle --parse-only: just print summary and exit
    if cli.parse_only {
        if suppress_stdout {
            return Ok(());
        }
        if json_mode {
            // Emit JSON summary for programmatic use (e.g. benchmarks/parsers/compare.py)
            let bus_pd_mw = network.bus_load_p_mw();
            let bus_qd_mvar = network.bus_load_q_mvar();
            let buses_json: Vec<serde_json::Value> = network
                .buses
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let bus_type_code = match b.bus_type {
                        surge_network::network::BusType::PQ => 1,
                        surge_network::network::BusType::PV => 2,
                        surge_network::network::BusType::Slack => 3,
                        surge_network::network::BusType::Isolated => 4,
                    };
                    serde_json::json!({
                        "number": b.number,
                        "bus_type_code": bus_type_code,
                        "base_kv": b.base_kv,
                        "vm": b.voltage_magnitude_pu,
                        "va_deg": b.voltage_angle_rad.to_degrees(),
                        "pd": bus_pd_mw[i],
                        "qd": bus_qd_mvar[i],
                        "bs": b.shunt_susceptance_mvar,
                        "gs": b.shunt_conductance_mw,
                    })
                })
                .collect();
            let branches_json: Vec<serde_json::Value> = network
                .branches
                .iter()
                .map(|br| {
                    serde_json::json!({
                        "from_bus": br.from_bus,
                        "to_bus": br.to_bus,
                        "circuit": br.circuit,
                        "r": br.r,
                        "x": br.x,
                        "b": br.b,
                        "tap": br.tap,
                        "shift_deg": br.phase_shift_rad.to_degrees(),
                        "in_service": br.in_service,
                    })
                })
                .collect();
            let summary = serde_json::json!({
                "n_buses": network.n_buses(),
                "n_branches": network.n_branches(),
                "n_generators": network.generators.len(),
                "total_load_mw": network.total_load_mw(),
                "total_gen_mw": network.total_generation_mw(),
                "base_mva": network.base_mva,
                "buses": buses_json,
                "branches": branches_json,
            });
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            println!("Parse complete (--parse-only).");
            println!(
                "{} buses, {} branches, {} generators",
                network.n_buses(),
                network.n_branches(),
                network.generators.len()
            );
        }
        return Ok(());
    }

    // Handle --convert: write network to another format without solving
    if let Some(ref convert_path) = cli.convert {
        let fmt_override = cli.export_format.as_deref();
        save_network(&network, convert_path, fmt_override)
            .with_context(|| format!("failed to convert to: {}", convert_path.display()))?;
        if !suppress_stdout && !json_mode {
            println!("Converted to: {}", convert_path.display());
        }
        return Ok(());
    }

    // Track the full solved-state artifact for --export.
    let mut solved_result: Option<SolvedStateResult> = None;

    match cli.method {
        CliMethod::Dcpf => {
            let dc_options = surge_dc::DcPfOptions {
                angle_reference: cli.angle_reference.into_runtime(),
                ..Default::default()
            };
            let result = surge_dc::solve_dc_opts(&network, &dc_options)
                .with_context(|| "DC power flow solve failed")?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        let solution = surge_dc::to_pf_solution(&result, &network);
                        print_json_result(&solution);
                    }
                    TextOrJson::Text => {
                        println!("\n--- DC Power Flow Results ---");
                        println!("Solve time: {:.3} ms", result.solve_time_secs * 1000.0);
                        println!(
                            "Slack bus injection: {:.4} p.u. ({:.1} MW)",
                            result.slack_p_injection,
                            result.slack_p_injection * network.base_mva
                        );
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_angle_range_summary(&network, &result.theta);
                                let loading_pct: Vec<f64> = network
                                    .branches
                                    .iter()
                                    .zip(result.branch_p_flow.iter())
                                    .map(|(branch, &pf_pu)| {
                                        if branch.rating_a_mva > 0.0 {
                                            (pf_pu * network.base_mva).abs() / branch.rating_a_mva
                                                * 100.0
                                        } else {
                                            f64::NAN
                                        }
                                    })
                                    .collect();
                                print_branch_loading_summary(&network, &loading_pct, 5);
                            }
                            ResolvedTextDetail::Full => {
                                println!("\nBus Voltage Angles:");
                                println!(
                                    "{:>6}  {:>10}  {:>10}",
                                    "Bus", "Angle(deg)", "Angle(rad)"
                                );
                                for (i, bus) in network.buses.iter().enumerate() {
                                    println!(
                                        "{:>6}  {:>10.4}  {:>10.6}",
                                        bus.number,
                                        result.theta[i].to_degrees(),
                                        result.theta[i]
                                    );
                                }

                                println!("\nBranch Flows:");
                                println!(
                                    "{:>6} {:>6}  {:>10}  {:>10}",
                                    "From", "To", "P(p.u.)", "P(MW)"
                                );
                                for (i, branch) in network.branches.iter().enumerate() {
                                    if !branch.in_service {
                                        continue;
                                    }
                                    println!(
                                        "{:>6} {:>6}  {:>10.4}  {:>10.1}",
                                        branch.from_bus,
                                        branch.to_bus,
                                        result.branch_p_flow[i],
                                        result.branch_p_flow[i] * network.base_mva
                                    );
                                }
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::PowerFlow(surge_dc::to_pf_solution(
                &result, &network,
            )));
        }
        CliMethod::Acpf => {
            let acpf_options = surge_ac::AcPfOptions {
                tolerance: cli.tolerance,
                max_iterations: cli.max_iter,
                flat_start: cli.flat_start,
                // True flat start means Va=0 for all non-slack buses (MATPOWER convention).
                // DC warm-start overwrites angles with DC-PF values, which can lead to a
                // different NR attractor on networks with multiple stable equilibria.  When
                // the user explicitly requests --flat-start, honour it completely by disabling
                // DC warm-start.  Warm-start mode (no --flat-start flag) still uses DC angles.
                dc_warm_start: !cli.flat_start,
                enforce_q_limits: !cli.no_q_limits,
                q_sharing: cli.q_sharing.into_runtime(),
                enforce_interchange: cli.enforce_interchange,
                angle_reference: cli.angle_reference.into_runtime(),
                ..Default::default()
            };

            // Try with specified start, fall back to flat start, then FDPF warm-start.
            // solve_ac_pf_with_dc_lines handles DC lines and FACTS devices; falls through to solve_ac_pf_kernel
            // with zero overhead when neither is present in the network.
            let solution = match surge_ac::solve_ac_pf(&network, &acpf_options) {
                Ok(sol) => sol,
                Err(_) if !cli.flat_start => {
                    let flat_opts = surge_ac::AcPfOptions {
                        flat_start: true,
                        ..acpf_options.clone()
                    };
                    match surge_ac::solve_ac_pf(&network, &flat_opts) {
                        Ok(sol) => sol,
                        Err(_) => {
                            // FDPF fallback: use fast decoupled PF to get approximate
                            // voltages, then retry NR with that as warm start.
                            try_fdpf_then_nr_fallback(&network, &acpf_options).with_context(
                                || {
                                    "Power flow did not converge. \
                                     Try --flat-start or check for isolated buses, \
                                     voltage collapse, or misconfigured generator limits."
                                },
                            )?
                        }
                    }
                }
                Err(_) => {
                    // Already flat start — try FDPF fallback directly
                    try_fdpf_then_nr_fallback(&network, &acpf_options).with_context(|| {
                        "Power flow did not converge (flat start). \
                             Check for isolated buses, voltage collapse, \
                             or misconfigured generator limits."
                    })?
                }
            };

            solved_result = Some(SolvedStateResult::PowerFlow(solution.clone()));

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&solution);
                    }
                    TextOrJson::Text => {
                        println!("\n--- AC Power Flow Results (Newton-Raphson) ---");
                        println!("Status: {:?}", solution.status);
                        println!("Iterations: {}", solution.iterations);
                        println!("Max mismatch: {:.2e} p.u.", solution.max_mismatch);
                        println!("Solve time: {:.3} ms", solution.solve_time_secs * 1000.0);
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_voltage_range_summary(
                                    &network,
                                    &solution.voltage_magnitude_pu,
                                );
                                print_angle_range_summary(&network, &solution.voltage_angle_rad);
                                let loading_pct =
                                    solution.branch_loading_pct(&network).unwrap_or_default();
                                print_branch_loading_summary(&network, &loading_pct, 5);
                            }
                            ResolvedTextDetail::Full => {
                                print_bus_voltage_table(
                                    &network,
                                    &solution.voltage_magnitude_pu,
                                    &solution.voltage_angle_rad,
                                    Some(&solution.active_power_injection_pu),
                                );
                            }
                        }
                    }
                }
            }
        }
        CliMethod::AcpfWarm => {
            // DC-initialized AC power flow: run DC PF first, use angles to warm-start NR.
            let dc_result = surge_dc::solve_dc(&network)
                .with_context(|| "DC power flow (warm-start phase) failed")?;

            // Build warm-start state: Vm from case data, Va from DC angles
            let warm = surge_ac::WarmStart {
                vm: network
                    .buses
                    .iter()
                    .map(|b| b.voltage_magnitude_pu)
                    .collect(),
                va: dc_result.theta.clone(),
            };

            let acpf_options = surge_ac::AcPfOptions {
                tolerance: cli.tolerance,
                max_iterations: cli.max_iter,
                flat_start: false,
                dc_warm_start: false,
                warm_start: Some(warm),
                enforce_q_limits: !cli.no_q_limits,
                q_sharing: cli.q_sharing.into_runtime(),
                enforce_interchange: cli.enforce_interchange,
                angle_reference: cli.angle_reference.into_runtime(),
                ..Default::default()
            };

            let solution = match surge_ac::solve_ac_pf(&network, &acpf_options) {
                Ok(sol) => sol,
                Err(_) => {
                    // DC warm-start failed — try FDPF fallback
                    try_fdpf_then_nr_fallback(&network, &acpf_options)
                        .with_context(|| "Newton-Raphson (KLU) warm-start power flow failed")?
                }
            };

            // Report total time (DC + NR) and keep one canonical final result.
            let nr_solve_time_secs = solution.solve_time_secs;
            let total_time_secs = dc_result.solve_time_secs + nr_solve_time_secs;
            let mut final_solution = solution.clone();
            final_solution.solve_time_secs = total_time_secs;

            solved_result = Some(SolvedStateResult::PowerFlow(final_solution.clone()));

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&final_solution);
                    }
                    TextOrJson::Text => {
                        println!("\n--- AC Power Flow Results (NR, DC Warm Start) ---");
                        println!("Status: {:?}", final_solution.status);
                        println!("Iterations: {}", final_solution.iterations);
                        println!("Max mismatch: {:.2e} p.u.", final_solution.max_mismatch);
                        println!(
                            "DC warm-up time: {:.3} ms",
                            dc_result.solve_time_secs * 1000.0
                        );
                        println!("NR solve time: {:.3} ms", nr_solve_time_secs * 1000.0);
                        println!("Total solve time: {:.3} ms", total_time_secs * 1000.0);
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_voltage_range_summary(
                                    &network,
                                    &final_solution.voltage_magnitude_pu,
                                );
                                print_angle_range_summary(
                                    &network,
                                    &final_solution.voltage_angle_rad,
                                );
                                let loading_pct = final_solution
                                    .branch_loading_pct(&network)
                                    .unwrap_or_default();
                                print_branch_loading_summary(&network, &loading_pct, 5);
                            }
                            ResolvedTextDetail::Full => {
                                print_bus_voltage_table(
                                    &network,
                                    &final_solution.voltage_magnitude_pu,
                                    &final_solution.voltage_angle_rad,
                                    Some(&final_solution.active_power_injection_pu),
                                );
                            }
                        }
                    }
                }
            }
        }
        CliMethod::Contingency => {
            let options = surge_contingency::ContingencyOptions {
                acpf_options: surge_ac::AcPfOptions {
                    tolerance: cli.tolerance,
                    max_iterations: cli.max_iter,
                    flat_start: cli.flat_start,
                    enforce_q_limits: !cli.no_q_limits,
                    q_sharing: cli.q_sharing.into_runtime(),
                    enforce_interchange: cli.enforce_interchange,
                    ..Default::default()
                },
                screening: cli.screening.into_runtime(),
                thermal_threshold_frac: cli.thermal_threshold / 100.0,
                store_post_voltages: cli.store_voltages,
                contingency_flat_start: cli.cont_flat_start,
                discrete_controls: cli.discrete_controls,
                voltage_stress_mode: cli.voltage_stress_mode.into_runtime(cli.l_index_threshold),
                ..Default::default()
            };

            let result = surge_contingency::analyze_n1_branch(&network, &options)
                .with_context(|| "contingency analysis failed")?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        // Pre-compute true violation counts (individual violations, not
                        // contingencies-with-violations) to match the Python surface.
                        let n_violations_thermal: usize = result
                            .results
                            .iter()
                            .flat_map(|r| r.violations.iter())
                            .filter(|v| {
                                matches!(v, surge_contingency::Violation::ThermalOverload { .. })
                            })
                            .count();
                        let n_violations_voltage: usize = result
                            .results
                            .iter()
                            .flat_map(|r| r.violations.iter())
                            .filter(|v| {
                                matches!(
                                    v,
                                    surge_contingency::Violation::VoltageLow { .. }
                                        | surge_contingency::Violation::VoltageHigh { .. }
                                )
                            })
                            .count();
                        let n_violations_total: usize =
                            result.results.iter().map(|r| r.violations.len()).sum();

                        let mut v = match serde_json::to_value(&result) {
                            Ok(v) => v,
                            Err(_) => {
                                // NaN/Inf in post-contingency results — use the
                                // standard diverged-result handler which prints
                                // structured JSON and exits with code 2.
                                print_json_result(&result);
                                unreachable!();
                            }
                        };
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "n_contingencies".into(),
                                serde_json::json!(result.summary.total_contingencies),
                            );
                            obj.insert(
                                "n_contingencies_with_violations".into(),
                                serde_json::json!(result.summary.with_violations),
                            );
                            obj.insert(
                                "solve_time_secs".into(),
                                serde_json::json!(result.summary.solve_time_secs),
                            );
                            obj.insert(
                                "n_violations_thermal".into(),
                                serde_json::json!(n_violations_thermal),
                            );
                            obj.insert(
                                "n_violations_voltage".into(),
                                serde_json::json!(n_violations_voltage),
                            );
                            obj.insert(
                                "n_violations_total".into(),
                                serde_json::json!(n_violations_total),
                            );
                        }
                        print_json_result(&v);
                    }
                    TextOrJson::Text => {
                        // Pre-compute true violation counts (individual violations, not
                        // contingencies-with-violations) to match the Python surface.
                        let n_viol_thermal: usize = result
                            .results
                            .iter()
                            .flat_map(|r| r.violations.iter())
                            .filter(|v| {
                                matches!(v, surge_contingency::Violation::ThermalOverload { .. })
                            })
                            .count();
                        let n_viol_voltage: usize = result
                            .results
                            .iter()
                            .flat_map(|r| r.violations.iter())
                            .filter(|v| {
                                matches!(
                                    v,
                                    surge_contingency::Violation::VoltageLow { .. }
                                        | surge_contingency::Violation::VoltageHigh { .. }
                                )
                            })
                            .count();
                        let n_viol_total: usize =
                            result.results.iter().map(|r| r.violations.len()).sum();

                        println!("\n--- N-1 Contingency Analysis ---");
                        println!(
                            "Total contingencies: {}",
                            result.summary.total_contingencies
                        );
                        println!("Screened out:        {}", result.summary.screened_out);
                        println!("AC solved:           {}", result.summary.ac_solved);
                        println!("Converged:           {}", result.summary.converged);
                        println!("With violations:     {}", result.summary.with_violations);
                        println!("Thermal violations:  {}", n_viol_thermal);
                        println!("Voltage violations:  {}", n_viol_voltage);
                        println!("Total violations:    {}", n_viol_total);
                        println!(
                            "Solve time:          {:.3} s",
                            result.summary.solve_time_secs
                        );

                        // Print contingencies with violations, sorted by severity
                        let mut violated: Vec<_> = result
                            .results
                            .iter()
                            .filter(|r| !r.violations.is_empty())
                            .collect();
                        violated.sort_by(|a, b| {
                            let a_max = max_severity(a);
                            let b_max = max_severity(b);
                            b_max
                                .partial_cmp(&a_max)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });

                        if violated.is_empty() {
                            println!("\nNo violations detected.");
                        } else {
                            println!(
                                "\n{:<30} {:>6} {:>10}  Worst violation",
                                "Contingency", "Conv?", "Iters"
                            );
                            println!("{}", "-".repeat(75));
                            for r in &violated {
                                let worst = worst_violation_summary(r);
                                println!(
                                    "{:<30} {:>6} {:>10}  {}",
                                    truncate(&r.label, 30),
                                    if r.converged { "yes" } else { "NO" },
                                    r.iterations,
                                    worst
                                );
                            }
                        }
                    }
                }
            }
        }
        CliMethod::DcOpf => {
            let opf_options = surge_opf::DcOpfOptions {
                tolerance: cli.tolerance,
                max_iterations: cli.max_iter,
                use_pwl_costs: cli.dc_cost_mode == Some(CliDcCostMode::Lp),
                pwl_cost_breakpoints: cli.dc_pwl_breakpoints,
                gen_limit_penalty: cli.gen_limit_penalty,
                use_loss_factors: cli.use_loss_factors,
                max_loss_iter: cli.loss_iterations,
                loss_tol: cli.loss_tolerance,
                ..Default::default()
            };
            let dc_runtime = match lp_solver.clone() {
                Some(solver) => surge_opf::DcOpfRuntime::default().with_lp_solver(solver),
                None => surge_opf::DcOpfRuntime::default(),
            };

            let dc_result =
                surge_opf::solve_dc_opf_with_runtime(&network, &opf_options, &dc_runtime)
                    .with_context(|| {
                        "DC-OPF is infeasible or failed. \
                     Check generator limits (pmin/pmax) and total load vs. capacity."
                    })?;
            let sol = &dc_result.opf;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&dc_opf_json_envelope(&dc_result));
                    }
                    TextOrJson::Text => {
                        println!("\n--- DC-OPF Results ---");
                        println!("Total cost: {:.2} $/hr", sol.total_cost);
                        println!("Solve time: {:.3} ms", sol.solve_time_secs * 1000.0);
                        println!("Iterations: {}", format_optional_iterations(sol.iterations));
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_solver_identity(
                                    sol.solver_name.as_deref(),
                                    sol.solver_version.as_deref(),
                                );
                                print_power_balance_summary(
                                    sol.total_generation_mw,
                                    sol.total_load_mw,
                                    sol.total_losses_mw,
                                );
                                print_generator_dispatch_summary(
                                    &network,
                                    &sol.generators.gen_p_mw,
                                    5,
                                );
                                print_lmp_summary(&network, &sol.pricing.lmp, 5);
                                print_branch_loading_summary(
                                    &network,
                                    &sol.branches.branch_loading_pct,
                                    5,
                                );
                            }
                            ResolvedTextDetail::Full => {
                                print_generator_dispatch_table(
                                    &network,
                                    &sol.generators.gen_p_mw,
                                    None,
                                );

                                println!("\nLocational Marginal Prices:");
                                println!(
                                    "{:>6}  {:>10}  {:>12}  {:>10}",
                                    "Bus", "LMP($/MWh)", "Energy", "Congest"
                                );
                                let bus_pd_mw = network.bus_load_p_mw();
                                for (i, bus) in network.buses.iter().enumerate() {
                                    if sol.pricing.lmp[i].abs() > 1e-4 || bus_pd_mw[i] > 0.0 {
                                        println!(
                                            "{:>6}  {:>10.4}  {:>12.4}  {:>10.4}",
                                            bus.number,
                                            sol.pricing.lmp[i],
                                            sol.pricing.lmp[i] - sol.pricing.lmp_congestion[i],
                                            sol.pricing.lmp_congestion[i]
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::DcOpf(dc_result.clone()));
        }
        CliMethod::Scopf => {
            let formulation = cli.scopf_formulation.into_runtime();
            let mode = cli.scopf_mode.into_runtime();

            let nlp_solver = if formulation == surge_opf::ScopfFormulation::Ac {
                make_ac_opf_nlp_solver(cli.solver)?
            } else {
                None
            };

            let scopf_options = surge_opf::ScopfOptions {
                formulation,
                mode,
                max_iterations: cli.scopf_max_iter,
                violation_tolerance_pu: cli.scopf_viol_tol,
                max_cuts_per_iteration: cli.scopf_max_cuts,
                contingency_rating: cli.contingency_rating.into_runtime(),
                enforce_flowgates: !cli.no_flowgates,
                enforce_angle_limits: !cli.no_angle_limits,
                dc_opf: surge_opf::DcOpfOptions {
                    tolerance: cli.tolerance,
                    max_iterations: cli.max_iter,
                    // SCOPF defaults to PWL (LP) costs — the HiGHS QP solver
                    // has numerical issues on large cases.  Use --dc-cost-mode qp
                    // to override with exact quadratic costs.
                    use_pwl_costs: cli.dc_cost_mode != Some(CliDcCostMode::Qp),
                    pwl_cost_breakpoints: cli.dc_pwl_breakpoints,
                    gen_limit_penalty: cli.gen_limit_penalty,
                    use_loss_factors: cli.use_loss_factors,
                    max_loss_iter: cli.loss_iterations,
                    loss_tol: cli.loss_tolerance,
                    ..Default::default()
                },
                ac: surge_opf::ScopfAcSettings {
                    opf: surge_opf::AcOpfOptions {
                        tolerance: cli.tolerance,
                        exact_hessian: true,
                        ..Default::default()
                    },
                    nr_convergence_tolerance: cli.tolerance,
                    enforce_voltage_security: !cli.no_voltage_security,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut scopf_runtime = surge_opf::ScopfRuntime::default();
            if let Some(lp_solver) = lp_solver.clone() {
                scopf_runtime = scopf_runtime.with_lp_solver(lp_solver);
            }
            if let Some(nlp_solver) = nlp_solver {
                scopf_runtime = scopf_runtime.with_nlp_solver(nlp_solver);
            }

            let sol = surge_opf::solve_scopf_with_runtime(&network, &scopf_options, &scopf_runtime)
                .with_context(|| "SCOPF solve failed")?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&sol);
                    }
                    TextOrJson::Text => {
                        println!(
                            "\n--- SCOPF Results ({:?} {:?}) ---",
                            sol.formulation, sol.mode
                        );
                        println!("Converged: {}", sol.converged);
                        println!("Total cost: {:.2} $/hr", sol.base_opf.total_cost);
                        println!("Solve time: {:.3} s", sol.solve_time_secs);
                        println!("Iterations: {}", sol.iterations);
                        println!(
                            "Contingencies evaluated: {}",
                            sol.total_contingencies_evaluated
                        );
                        println!(
                            "Contingency constraints: {}",
                            sol.total_contingency_constraints
                        );

                        print_generator_dispatch_table(
                            &network,
                            &sol.base_opf.generators.gen_p_mw,
                            None,
                        );

                        println!("\nLocational Marginal Prices:");
                        if !sol.lmp_contingency_congestion.is_empty() {
                            println!(
                                "{:>6}  {:>10}  {:>10}  {:>10}  {:>10}",
                                "Bus", "LMP($/MWh)", "Energy", "BaseCong", "CtgCong"
                            );
                            let bus_pd_mw = network.bus_load_p_mw();
                            for (i, bus) in network.buses.iter().enumerate() {
                                if sol.base_opf.pricing.lmp[i].abs() > 1e-4 || bus_pd_mw[i] > 0.0 {
                                    let energy = sol.base_opf.pricing.lmp[i]
                                        - sol.base_opf.pricing.lmp_congestion[i];
                                    let base_cong = sol.base_opf.pricing.lmp_congestion[i]
                                        - sol.lmp_contingency_congestion[i];
                                    let ctg_cong = sol.lmp_contingency_congestion[i];
                                    println!(
                                        "{:>6}  {:>10.4}  {:>10.4}  {:>10.4}  {:>10.4}",
                                        bus.number,
                                        sol.base_opf.pricing.lmp[i],
                                        energy,
                                        base_cong,
                                        ctg_cong
                                    );
                                }
                            }
                        } else {
                            println!("{:>6}  {:>10}  {:>10}", "Bus", "LMP($/MWh)", "Energy");
                            let bus_pd_mw = network.bus_load_p_mw();
                            for (i, bus) in network.buses.iter().enumerate() {
                                if sol.base_opf.pricing.lmp[i].abs() > 1e-4 || bus_pd_mw[i] > 0.0 {
                                    let energy = sol.base_opf.pricing.lmp[i]
                                        - sol.base_opf.pricing.lmp_congestion[i];
                                    println!(
                                        "{:>6}  {:>10.4}  {:>10.4}",
                                        bus.number, sol.base_opf.pricing.lmp[i], energy
                                    );
                                }
                            }
                        }

                        if !sol.binding_contingencies.is_empty() {
                            println!("\nBinding Contingencies:");
                            println!(
                                "{:<35} {:>8} {:>8} {:>10} {:>10}",
                                "Contingency", "OutBr", "MonBr", "Load(%)", "Shadow"
                            );
                            for bc in &sol.binding_contingencies {
                                let out_str = bc
                                    .outaged_branch_indices
                                    .iter()
                                    .map(|i| i.to_string())
                                    .collect::<Vec<_>>()
                                    .join(",");
                                println!(
                                    "{:<35} {:>8} {:>8} {:>10.1} {:>10.4}",
                                    truncate(&bc.contingency_label, 35),
                                    out_str,
                                    bc.monitored_branch_idx,
                                    bc.loading_pct,
                                    bc.shadow_price
                                );
                            }
                        }

                        if !sol.remaining_violations.is_empty() {
                            println!("\nRemaining Violations:");
                            for v in &sol.remaining_violations {
                                println!(
                                    "  {} — thermal: {}, voltage: {}",
                                    v.contingency_label,
                                    v.thermal_violations.len(),
                                    v.voltage_violations.len()
                                );
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::Scopf(sol.clone()));
        }
        CliMethod::AcOpf => {
            let nlp_solver = make_ac_opf_nlp_solver(cli.solver)?;
            // --ac-opf-max-iter 0 = auto-scale; else use explicit value.
            // Falls back to --max-iter if ac_opf_max_iter is 0 and max_iter != default (500).
            let ac_opf_max_iter = if cli.ac_opf_max_iter != 0 {
                cli.ac_opf_max_iter
            } else if cli.max_iter != 500 {
                cli.max_iter // user set --max-iter explicitly
            } else {
                0 // auto
            };
            let dc_opf_warm_start = match cli.dc_opf_warm_start {
                DcOpfWarmStart::Yes => Some(true),
                DcOpfWarmStart::No => Some(false),
                DcOpfWarmStart::Auto => None,
            };
            // HVDC: if --include-hvdc is set or network has HVDC data, enable it;
            // otherwise leave as None (no HVDC).
            let has_hvdc_data = cli.include_hvdc || !network.hvdc.is_empty();
            let include_hvdc = if has_hvdc_data { Some(true) } else { None };
            let discrete_mode = cli.ac_discrete_mode.into_runtime();
            let opf_options = surge_opf::AcOpfOptions {
                tolerance: cli.tolerance,
                max_iterations: ac_opf_max_iter,
                exact_hessian: true,
                include_hvdc,
                enforce_capability_curves: !cli.no_capability_curves,
                discrete_mode,
                optimize_svc: cli.optimize_svc,
                optimize_tcsc: cli.optimize_tcsc,
                ..Default::default()
            };
            let mut runtime = surge_opf::AcOpfRuntime::default();
            if let Some(nlp_solver) = nlp_solver {
                runtime = runtime.with_nlp_solver(nlp_solver);
            }
            if let Some(enabled) = dc_opf_warm_start {
                runtime = runtime.with_dc_opf_warm_start(enabled);
            }

            let sol = surge_opf::solve_ac_opf_with_runtime(&network, &opf_options, &runtime)
                .with_context(|| {
                    "AC-OPF is infeasible or failed to converge. \
                     Check generator limits (pmin/pmax/qmin/qmax), voltage bounds, \
                     and total load vs. capacity."
                })?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&ac_opf_json_envelope(&sol));
                    }
                    TextOrJson::Text => {
                        println!("\n--- AC-OPF Results ---");
                        println!("Total cost: {:.2} $/hr", sol.total_cost);
                        println!("Solve time: {:.3} ms", sol.solve_time_secs * 1000.0);
                        println!("Iterations: {}", format_optional_iterations(sol.iterations));
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_solver_identity(
                                    sol.solver_name.as_deref(),
                                    sol.solver_version.as_deref(),
                                );
                                print_power_balance_summary(
                                    sol.total_generation_mw,
                                    sol.total_load_mw,
                                    sol.total_losses_mw,
                                );
                                print_voltage_range_summary(
                                    &network,
                                    &sol.power_flow.voltage_magnitude_pu,
                                );
                                print_angle_range_summary(
                                    &network,
                                    &sol.power_flow.voltage_angle_rad,
                                );
                                print_generator_dispatch_summary(
                                    &network,
                                    &sol.generators.gen_p_mw,
                                    5,
                                );
                                print_lmp_summary(&network, &sol.pricing.lmp, 5);
                                print_branch_loading_summary(
                                    &network,
                                    &sol.branches.branch_loading_pct,
                                    5,
                                );
                                if let Some(feasible) = sol.devices.discrete_feasible {
                                    println!(
                                        "Discrete round-and-check: {}",
                                        if feasible {
                                            "feasible"
                                        } else {
                                            "violations detected"
                                        }
                                    );
                                }
                            }
                            ResolvedTextDetail::Full => {
                                print_generator_dispatch_table(
                                    &network,
                                    &sol.generators.gen_p_mw,
                                    Some(&sol.generators.gen_q_mvar),
                                );

                                println!("\nBus Voltages:");
                                println!("{:>6}  {:>10}  {:>10}", "Bus", "Vm(p.u.)", "Va(deg)");
                                for (i, bus) in network.buses.iter().enumerate() {
                                    println!(
                                        "{:>6}  {:>10.6}  {:>10.4}",
                                        bus.number,
                                        sol.power_flow.voltage_magnitude_pu[i],
                                        sol.power_flow.voltage_angle_rad[i].to_degrees()
                                    );
                                }

                                println!("\nLocational Marginal Prices:");
                                println!(
                                    "{:>6}  {:>10}  {:>10}  {:>10}  {:>10}",
                                    "Bus", "LMP($/MWh)", "Congest", "Loss", "Q-LMP"
                                );
                                let bus_pd_mw = network.bus_load_p_mw();
                                for (i, bus) in network.buses.iter().enumerate() {
                                    if sol.pricing.lmp[i].abs() > 1e-4 || bus_pd_mw[i] > 0.0 {
                                        let q_lmp =
                                            sol.pricing.lmp_reactive.get(i).copied().unwrap_or(0.0);
                                        println!(
                                            "{:>6}  {:>10.4}  {:>10.4}  {:>10.4}  {:>10.4}",
                                            bus.number,
                                            sol.pricing.lmp[i],
                                            sol.pricing.lmp_congestion[i],
                                            sol.pricing.lmp_loss[i],
                                            q_lmp
                                        );
                                    }
                                }

                                // Show branch loading for constrained branches
                                let flows = sol.power_flow.branch_apparent_power();
                                let mut loaded: Vec<(usize, f64, f64)> = network
                                    .branches
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, br)| br.in_service && br.rating_a_mva >= 1.0)
                                    .map(|(l, br)| {
                                        (l, flows[l], flows[l] / br.rating_a_mva * 100.0)
                                    })
                                    .filter(|(_, _, pct)| *pct > 50.0)
                                    .collect();
                                loaded.sort_by(|a, b| b.2.total_cmp(&a.2));

                                if !loaded.is_empty() {
                                    println!("\nBranch Loading (>50%):");
                                    println!(
                                        "{:>6} {:>6}  {:>10}  {:>10}  {:>8}",
                                        "From", "To", "|S|(MVA)", "Rate(MVA)", "Load(%)"
                                    );
                                    for (l, flow, pct) in loaded.iter().take(20) {
                                        let br = &network.branches[*l];
                                        println!(
                                            "{:>6} {:>6}  {:>10.1}  {:>10.1}  {:>8.1}",
                                            br.from_bus, br.to_bus, flow, br.rating_a_mva, pct
                                        );
                                    }
                                }

                                // Discrete round-and-check results
                                if let Some(feasible) = sol.devices.discrete_feasible {
                                    println!(
                                        "\nDiscrete Round-and-Check: {}",
                                        if feasible {
                                            "FEASIBLE"
                                        } else {
                                            "VIOLATIONS DETECTED"
                                        }
                                    );
                                    if !sol.devices.tap_dispatch.is_empty() {
                                        println!(
                                            "  Tap dispatch ({} branches):",
                                            sol.devices.tap_dispatch.len()
                                        );
                                        for &(br_idx, cont, rounded) in &sol.devices.tap_dispatch {
                                            let br = &network.branches[br_idx];
                                            println!(
                                                "    Branch {} ({}-{}): {:.5} -> {:.5}",
                                                br_idx, br.from_bus, br.to_bus, cont, rounded
                                            );
                                        }
                                    }
                                    if !sol.devices.phase_dispatch.is_empty() {
                                        println!(
                                            "  Phase dispatch ({} branches):",
                                            sol.devices.phase_dispatch.len()
                                        );
                                        for &(br_idx, cont, rounded) in &sol.devices.phase_dispatch
                                        {
                                            let br = &network.branches[br_idx];
                                            println!(
                                                "    Branch {} ({}-{}): {:.4} -> {:.4} rad",
                                                br_idx, br.from_bus, br.to_bus, cont, rounded
                                            );
                                        }
                                    }
                                    for v in &sol.devices.discrete_violations {
                                        println!("  VIOLATION: {}", v);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::Opf(sol.clone()));
        }
        CliMethod::Hvdc => {
            let opts = surge_hvdc::HvdcOptions {
                method: cli.hvdc_method.into_runtime(),
                tol: cli.tolerance,
                max_iter: cli.max_iter,
                ac_tol: cli.tolerance,
                dc_tol: cli.hvdc_dc_tol,
                max_dc_iter: cli.hvdc_dc_max_iter,
                flat_start: cli.flat_start,
                ..Default::default()
            };

            let sol = surge_hvdc::solve_hvdc(&network, &opts)
                .map_err(|e| anyhow::anyhow!("HVDC solve failed: {}", e))?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&sol);
                    }
                    TextOrJson::Text => {
                        println!("\n=== HVDC Power Flow Solution ===");
                        println!(
                            "Converged: {}  Iterations: {}  Method: {:?}",
                            sol.converged, sol.iterations, sol.method
                        );
                        println!(
                            "Total losses: {:.2} MW  (converter {:.2} MW, dc network {:.2} MW)",
                            sol.total_loss_mw,
                            sol.total_converter_loss_mw,
                            sol.total_dc_network_loss_mw
                        );

                        println!("\nStation Results:");
                        println!(
                            "  {:>4}  {:>6}  {:>6}  {:>12}  {:>12}  {:>12}  {:>12}  {:>10}",
                            "Stn",
                            "AC Bus",
                            "DC Bus",
                            "P_ac(MW)",
                            "Q_ac(MVAr)",
                            "P_dc(MW)",
                            "Loss(MW)",
                            "V_dc(pu)"
                        );
                        for (i, station) in sol.stations.iter().enumerate() {
                            println!(
                                "  {:>4}  {:>6}  {:>6}  {:>12.2}  {:>12.2}  {:>12.2}  {:>12.2}  {:>10.4}",
                                i + 1,
                                station.ac_bus,
                                station
                                    .dc_bus
                                    .map(|dc_bus| dc_bus.to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                                station.p_ac_mw,
                                station.q_ac_mvar,
                                station.p_dc_mw,
                                station.converter_loss_mw,
                                station.v_dc_pu
                            );
                        }

                        if !sol.dc_buses.is_empty() {
                            println!("\nDC Bus Voltages:");
                            for bus in &sol.dc_buses {
                                println!("  DC bus {}: {:.6} pu", bus.dc_bus, bus.voltage_pu);
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::Hvdc(sol.clone()));
        }
        CliMethod::Fdpf => {
            let fdpf_opts = surge_ac::FdpfOptions {
                tolerance: cli.tolerance,
                max_iterations: cli.max_iter,
                flat_start: cli.flat_start,
                ..Default::default()
            };
            let solution =
                surge_ac::solve_fdpf(&network, &fdpf_opts).map_err(anyhow::Error::msg)?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&solution);
                    }
                    TextOrJson::Text => {
                        println!("\n--- AC Power Flow Results (Fast Decoupled) ---");
                        println!("Status: {:?}", solution.status);
                        println!("Iterations: {}", solution.iterations);
                        println!("Solve time: {:.3} ms", solution.solve_time_secs * 1000.0);
                        match text_detail {
                            ResolvedTextDetail::Summary => {
                                print_voltage_range_summary(
                                    &network,
                                    &solution.voltage_magnitude_pu,
                                );
                                print_angle_range_summary(&network, &solution.voltage_angle_rad);
                                let loading_pct =
                                    solution.branch_loading_pct(&network).unwrap_or_default();
                                print_branch_loading_summary(&network, &loading_pct, 5);
                            }
                            ResolvedTextDetail::Full => {
                                print_bus_voltage_table(
                                    &network,
                                    &solution.voltage_magnitude_pu,
                                    &solution.voltage_angle_rad,
                                    None,
                                );
                            }
                        }
                    }
                }
            }
            solved_result = Some(SolvedStateResult::PowerFlow(solution.clone()));
        }
        // ── N-2 (simultaneous double-branch contingency) ───────────────────────
        CliMethod::N2 => {
            let opts = surge_contingency::advanced::n2::N2Options {
                tolerance: cli.tolerance,
                max_iterations: cli.max_iter as usize,
                ..surge_contingency::advanced::n2::N2Options::default()
            };
            let result =
                surge_contingency::advanced::n2::run_n2_contingency_analysis(&network, &opts)
                    .map_err(|e| anyhow::anyhow!("N-2 contingency analysis failed: {e}"))?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&result);
                    }
                    TextOrJson::Text => {
                        println!("\n--- N-2 Double-Branch Contingency Analysis ---");
                        println!("Total pairs:           {}", result.total_pairs);
                        println!("Radial skipped:        {}", result.radial_skipped);
                        println!("Tier 1 screened:       {}", result.tier1_screened);
                        println!("Tier 1 flagged:        {}", result.tier1_violations);
                        println!("Tier 2 screened:       {}", result.tier2_screened);
                        println!("Tier 2 flagged:        {}", result.tier2_violations);
                        println!("Tier 3 clear:          {}", result.tier3_clear);
                        println!("Tier 3 violations:     {}", result.tier3_violations);
                        println!("Non-convergent:        {}", result.non_convergent);
                        println!("TPL compliant:         {}", result.tpl_compliant);
                        println!("Wall time: {:.3} s", result.solve_time_s);
                    }
                }
            }
        }

        // ── DSA — Dynamic Security Assessment ─────────────────────────────────
        // Runs N-1 transient stability screening (parallel) and reports the
        // security margin as the fraction of stable contingencies.
        CliMethod::Orpd => {
            let objective = parse_orpd_objective(&cli)?;
            let nlp_solver = make_nlp_solver(cli.solver)?;
            let opts = surge_opf::switching::OrpdOptions {
                objective,
                fix_pg: true,
                optimize_q: true,
                enforce_thermal_limits: true,
                tol: cli.tolerance,
                max_iter: cli.max_iter,
                print_level: 0,
                exact_hessian: true,
                nlp_solver,
                ..Default::default()
            };
            let result = surge_opf::switching::solve_orpd(&network, &opts)
                .map_err(|e| anyhow::anyhow!("ORPD solve failed: {e}"))?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        let mut v = match serde_json::to_value(&result) {
                            Ok(v) => v,
                            Err(_) => {
                                print_json_result(&result);
                                unreachable!();
                            }
                        };
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "objective_mode".into(),
                                serde_json::json!(opts.objective.canonical_name()),
                            );
                        }
                        print_json_result(&v);
                    }
                    TextOrJson::Text => {
                        println!("\n--- Optimal Reactive Power Dispatch (ORPD) ---");
                        println!("Converged:       {}", result.converged);
                        println!("Objective mode:  {}", opts.objective.canonical_name());
                        println!(
                            "Iterations:      {}",
                            format_optional_iterations(result.iterations)
                        );
                        println!("Objective:       {:.6} pu", result.objective);
                        println!("Total losses:    {:.2} MW", result.total_losses_mw);
                        println!("Voltage dev:     {:.6} pu RMS", result.voltage_deviation);
                        println!("Solve time:      {:.3} ms", result.solve_time_ms);
                        println!("\nReactive Dispatch (ORPD solution):");
                        println!("{:>6}  {:>10}  {:>10}", "Bus", "Pg(MW)", "Qg(MVAr)");
                        let gen_indices: Vec<usize> = network
                            .generators
                            .iter()
                            .enumerate()
                            .filter(|(_, g)| g.in_service)
                            .map(|(i, _)| i)
                            .collect();
                        let base = network.base_mva;
                        for (j, &gi) in gen_indices.iter().enumerate() {
                            let g = &network.generators[gi];
                            let pg = result.p_dispatch.get(j).copied().unwrap_or(0.0) * base;
                            let qg = result.q_dispatch.get(j).copied().unwrap_or(0.0) * base;
                            println!("{:>6}  {:>10.1}  {:>10.1}", g.bus, pg, qg);
                        }
                    }
                }
            }
        }

        CliMethod::Ots => {
            let ots_opts = surge_opf::switching::OtsOptions {
                formulation: surge_opf::switching::OtsFormulation::DcMilp,
                switchable_branches: surge_opf::switching::SwitchableSet::AllBranches,
                max_switches_open: None,
                mip_gap: 0.01,
                big_m: None,
                time_limit_s: 300.0,
                tolerance: cli.tolerance,
                max_iter: cli.max_iter,
            };
            let ots_runtime = match lp_solver.clone() {
                Some(solver) => surge_opf::switching::OtsRuntime::default().with_lp_solver(solver),
                None => surge_opf::switching::OtsRuntime::default(),
            };
            let result =
                surge_opf::switching::solve_ots_with_runtime(&network, &ots_opts, &ots_runtime)
                    .map_err(|e| {
                        anyhow::anyhow!("OTS (optimal transmission switching) failed: {e}")
                    })?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&result);
                    }
                    TextOrJson::Text => {
                        println!("\n--- Optimal Transmission Switching (OTS) ---");
                        println!("Converged:     {}", result.converged);
                        println!("Objective:     {:.2} $/hr", result.objective);
                        println!("Switches open: {}", result.n_switches);
                        println!("MIP gap:       {:.4}", result.mip_gap);
                        println!("Solve time:    {:.3} ms", result.solve_time_ms);
                        for (from, to, circuit) in &result.switched_out {
                            println!("  Opened: {} -> {} (circuit {})", from, to, circuit);
                        }
                    }
                }
            }
        }

        CliMethod::InjectionCapability => {
            use surge_transfer::injection::{
                InjectionCapabilityOptions, compute_injection_capability,
            };
            let options = InjectionCapabilityOptions {
                post_contingency_rating_fraction: cli.post_ctg_rating_frac,
                ..InjectionCapabilityOptions::default()
            };
            let result = compute_injection_capability(&network, &options)
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&result);
                    }
                    TextOrJson::Text => {
                        println!("\n--- Injection Capability (FERC Order 2023) ---");
                        println!(
                            "Post-contingency rating fraction: {:.2}",
                            cli.post_ctg_rating_frac
                        );
                        println!("Buses: {}\n", result.by_bus.len());

                        if !result.failed_contingencies.is_empty() {
                            eprintln!(
                                "WARNING: {} contingencies failed evaluation (bus limits conservatively zeroed):",
                                result.failed_contingencies.len()
                            );
                            for &k in &result.failed_contingencies {
                                eprintln!("  branch {k}");
                            }
                            eprintln!();
                        }

                        let mut sorted = result.by_bus.clone();
                        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

                        println!("{:>8}  {:>18}", "Bus", "Max Injection (MW)");
                        println!("{:>8}  {:>18}", "--------", "------------------");
                        for (bus, cap) in &sorted {
                            if cap.is_infinite() {
                                println!("{bus:>8}               inf");
                            } else {
                                println!("{bus:>8}  {cap:>18.2}");
                            }
                        }
                    }
                }
            }
        }

        CliMethod::NercAtc => {
            use surge_transfer::{
                AtcMargins, AtcOptions, NercAtcLimitCause, NercAtcRequest, TransferPath,
                compute_nerc_atc,
            };

            let source_buses = cli
                .source_buses
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--source-buses is required for nerc-atc method"))?;
            let sink_buses = cli
                .sink_buses
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--sink-buses is required for nerc-atc method"))?;

            let request = NercAtcRequest {
                path: TransferPath::new(
                    "cli_transfer".to_string(),
                    source_buses.clone(),
                    sink_buses.clone(),
                ),
                options: AtcOptions {
                    monitored_branches: None,
                    contingency_branches: None,
                    margins: AtcMargins::default(),
                },
            };

            let result =
                compute_nerc_atc(&network, &request).map_err(|e| anyhow::anyhow!("{e}"))?;

            if !suppress_stdout {
                match cli.output {
                    TextOrJson::Json => {
                        print_json_result(&result);
                    }
                    TextOrJson::Text => {
                        println!("\n--- NERC ATC (MOD-029/MOD-030) ---");
                        println!("Transfer path: {:?} → {:?}", source_buses, sink_buses);
                        println!("ATC:              {:>10.2} MW", result.atc_mw);
                        println!("TTC:              {:>10.2} MW", result.ttc_mw);
                        println!("TRM:              {:>10.2} MW", result.trm_mw);
                        println!("CBM:              {:>10.2} MW", result.cbm_mw);
                        println!("ETC:              {:>10.2} MW", result.etc_mw);
                        println!("Limit cause:      {:>10}", result.limit_cause);
                        match result.limit_cause {
                            NercAtcLimitCause::Unconstrained => {}
                            NercAtcLimitCause::BasecaseThermal { monitored_branch } => {
                                println!("Binding branch:   {:>10}", monitored_branch);
                            }
                            NercAtcLimitCause::ContingencyThermal {
                                monitored_branch,
                                contingency_branch,
                            } => {
                                println!("Binding branch:   {:>10}", monitored_branch);
                                println!("Binding ctg:      {:>10}", contingency_branch);
                            }
                            NercAtcLimitCause::FailClosedOutage { contingency_branch } => {
                                println!("Binding ctg:      {:>10}", contingency_branch);
                            }
                        }
                        if result.reactive_margin_warning {
                            println!("WARNING: reactive margin > 70% near transfer path");
                        }
                    }
                }
            }
        }
    }

    if let Some(ref export_path) = cli.export {
        let method_name = cli
            .method
            .to_possible_value()
            .map(|v| v.get_name().to_string())
            .unwrap_or_else(|| format!("{:?}", cli.method));
        let Some(result) = solved_result.clone() else {
            anyhow::bail!(
                "--export requires a method that produces a solved-state artifact; --method {method_name} does not"
            );
        };
        let artifact = SolvedStateArtifact {
            artifact_version: 1,
            method: method_name,
            network: network.clone(),
            result,
        };
        save_solved_state_artifact(export_path, &artifact).with_context(|| {
            format!(
                "failed to export solved artifact to: {}",
                export_path.display()
            )
        })?;
        if !suppress_stdout && !json_mode {
            println!("Exported solved artifact to: {}", export_path.display());
        }
    }

    Ok(())
}

/// FDPF fallback: run fast decoupled PF to get approximate voltages, then retry NR.
///
/// Used when NR fails to converge from both warm and flat starts.  FDPF has
/// linear convergence but is much more robust to poor initial conditions.
/// The FDPF result (even at relaxed tolerance) provides a good warm-start
/// for a second NR attempt.
fn try_fdpf_then_nr_fallback(
    network: &surge_network::Network,
    base_options: &surge_ac::AcPfOptions,
) -> Result<surge_solution::PfSolution> {
    let fdpf_opts = surge_ac::FdpfOptions {
        tolerance: 1e-4,
        max_iterations: 500,
        flat_start: false,
        enforce_q_limits: false, // warm-start only needs proximity, not accuracy
        ..Default::default()
    };
    let fdpf_sol = surge_ac::solve_fdpf(network, &fdpf_opts).map_err(anyhow::Error::msg)?;
    if fdpf_sol.status != surge_solution::SolveStatus::Converged {
        anyhow::bail!("FDPF fallback also failed to converge");
    }

    let retry_opts = surge_ac::AcPfOptions {
        warm_start: Some(surge_ac::WarmStart::from_solution(&fdpf_sol)),
        flat_start: false,
        ..base_options.clone()
    };

    surge_ac::solve_ac_pf(network, &retry_opts)
        .map_err(|e| anyhow::anyhow!("NR retry after FDPF warm-start failed: {e}"))
}

fn save_solved_state_artifact(
    path: &std::path::Path,
    artifact: &SolvedStateArtifact,
) -> Result<()> {
    let file = std::fs::File::create(path)?;
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zst"))
    {
        let mut encoder = zstd::stream::write::Encoder::new(file, 3)?;
        serde_json::to_writer_pretty(&mut encoder, artifact)?;
        encoder.finish()?;
    } else {
        serde_json::to_writer_pretty(file, artifact)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::network::{Bus, BusType, Network};

    #[test]
    fn save_solved_state_artifact_writes_network_and_result() {
        let mut network = Network::new("artifact-test");
        network.buses.push(Bus::new(1, BusType::Slack, 230.0));

        let solution = surge_solution::PfSolution {
            status: surge_solution::SolveStatus::Converged,
            voltage_magnitude_pu: vec![1.0],
            voltage_angle_rad: vec![0.0],
            bus_numbers: vec![1],
            ..Default::default()
        };
        let artifact = SolvedStateArtifact {
            artifact_version: 1,
            method: "acpf".to_string(),
            network: network.clone(),
            result: SolvedStateResult::PowerFlow(solution),
        };

        let path = std::env::temp_dir().join("surge_solved_artifact_test.json");
        let _ = std::fs::remove_file(&path);

        save_solved_state_artifact(&path, &artifact).expect("artifact export should succeed");

        let raw = std::fs::read_to_string(&path).expect("artifact file should exist");
        let json: serde_json::Value =
            serde_json::from_str(&raw).expect("artifact should be valid JSON");
        assert_eq!(json["artifact_version"], 1);
        assert_eq!(json["method"], "acpf");
        assert_eq!(json["network"]["name"], "artifact-test");
        assert_eq!(json["result_kind"], "power_flow");
        assert_eq!(json["result"]["status"], "Converged");

        let _ = std::fs::remove_file(&path);
    }
}
