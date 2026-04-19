// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Solve a short DC dispatch horizon on refreshed ACTIVSg bundles.
//!
//! Usage:
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 2000 1
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 2000 2
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 10k 1
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 2000 1 --linear-costs
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 2000 24 --pwl-costs --time-coupled
//!   cargo run -p surge-market --example activsg_dispatch_smoke -- 2000 1 /path/to/case.surge.json.zst /path/to/ACTIVSg_Time_Series

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, anyhow};
use surge_dispatch::datasets::{ActivsgCase, read_tamu_activsg_time_series};
use surge_dispatch::{
    CommitmentPolicy, DispatchMarket, DispatchModel, DispatchNetwork, DispatchRequest,
    FlowgatePolicy, GeneratorCostModeling, IntervalCoupling, ThermalLimitPolicy,
};
use surge_network::Network;
use surge_network::market::CostCurve;

fn main() -> anyhow::Result<()> {
    let mut linear_costs = false;
    let mut use_pwl_costs = false;
    let mut time_coupled = false;
    let mut pwl_cost_breakpoints = 20usize;
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--linear-costs" {
            linear_costs = true;
        } else if arg == "--pwl-costs" {
            use_pwl_costs = true;
        } else if arg == "--time-coupled" {
            time_coupled = true;
        } else if arg == "--pwl-breakpoints" {
            let raw = args
                .next()
                .ok_or_else(|| anyhow!("--pwl-breakpoints requires a value"))?;
            pwl_cost_breakpoints = raw
                .parse::<usize>()
                .with_context(|| format!("parse --pwl-breakpoints from `{raw}`"))?;
            if pwl_cost_breakpoints < 2 {
                return Err(anyhow!("--pwl-breakpoints must be >= 2"));
            }
        } else {
            positional.push(arg);
        }
    }
    if positional.len() > 4 {
        return Err(anyhow!(
            "usage: activsg_dispatch_smoke [CASE] [PERIODS] [CASE_PATH] [TIME_SERIES_ROOT] [--linear-costs] [--pwl-costs] [--time-coupled] [--pwl-breakpoints N]"
        ));
    }

    let case = match positional.first() {
        Some(raw) => parse_case(raw)?,
        None => ActivsgCase::Activsg2000,
    };
    let periods = positional
        .get(1)
        .map(String::as_str)
        .map(parse_periods)
        .transpose()?
        .unwrap_or(1);
    let case_path = positional
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_case_path(case));
    let time_series_root = positional
        .get(3)
        .map(PathBuf::from)
        .unwrap_or_else(default_time_series_root);

    let started = Instant::now();
    let network =
        surge_io::load(&case_path).with_context(|| format!("load {}", case_path.display()))?;
    let loaded_at = Instant::now();
    eprintln!(
        "loaded network in {:.3}s",
        loaded_at.duration_since(started).as_secs_f64()
    );
    io::stderr().flush().ok();
    let imported =
        read_tamu_activsg_time_series(&network, &time_series_root, case, &Default::default())
            .with_context(|| {
                format!(
                    "import ACTIVSg time series from {}",
                    time_series_root.display()
                )
            })?;
    let imported_at = Instant::now();
    eprintln!(
        "imported time series in {:.3}s",
        imported_at.duration_since(loaded_at).as_secs_f64()
    );
    io::stderr().flush().ok();
    let mut adjusted_network = imported.network_with_nameplate_overrides(&network)?;
    if linear_costs {
        override_with_linear_smoke_costs(&mut adjusted_network);
    } else {
        ensure_missing_costs(&mut adjusted_network);
    }
    let adjusted_at = Instant::now();
    eprintln!(
        "applied nameplate overrides in {:.3}s",
        adjusted_at.duration_since(imported_at).as_secs_f64()
    );
    io::stderr().flush().ok();
    let model = DispatchModel::prepare(&adjusted_network)?;
    let timeline = imported
        .timeline_for(periods)
        .map_err(|error| anyhow!(error.to_string()))?;
    let profiles = imported
        .dc_profiles(periods)
        .map_err(|error| anyhow!(error.to_string()))?;
    let request = DispatchRequest::builder()
        .dc()
        .commitment(CommitmentPolicy::AllCommitted)
        .coupling(if time_coupled {
            IntervalCoupling::TimeCoupled
        } else {
            IntervalCoupling::PeriodByPeriod
        })
        .timeline(timeline)
        .profiles(profiles)
        .market(DispatchMarket {
            generator_cost_modeling: use_pwl_costs.then_some(GeneratorCostModeling {
                use_pwl_costs: true,
                pwl_cost_breakpoints,
            }),
            ..DispatchMarket::default()
        })
        .network(DispatchNetwork {
            thermal_limits: ThermalLimitPolicy {
                enforce: false,
                ..ThermalLimitPolicy::default()
            },
            flowgates: FlowgatePolicy {
                enabled: false,
                ..FlowgatePolicy::default()
            },
            ..DispatchNetwork::default()
        })
        .build();

    let solve_started = Instant::now();
    eprintln!("starting DC dispatch solve for {periods} period(s)...");
    io::stderr().flush().ok();
    let solution = model.solve(&request)?;
    let solved_at = Instant::now();

    println!("Case:                 {:?}", case);
    println!("Periods solved:       {}", periods);
    println!("Linear costs:         {}", linear_costs);
    println!("PWL generator costs:  {}", use_pwl_costs);
    println!("PWL breakpoints:      {}", pwl_cost_breakpoints);
    println!("Time coupled:         {}", time_coupled);
    println!("Case path:            {}", case_path.display());
    println!("Time-series root:     {}", time_series_root.display());
    println!(
        "Load phase:           {:.3}s",
        loaded_at.duration_since(started).as_secs_f64()
    );
    println!(
        "Time-series import:   {:.3}s",
        imported_at.duration_since(loaded_at).as_secs_f64()
    );
    println!(
        "Nameplate adjust:     {:.3}s",
        adjusted_at.duration_since(imported_at).as_secs_f64()
    );
    println!(
        "Solve phase:          {:.3}s",
        solved_at.duration_since(solve_started).as_secs_f64()
    );
    println!(
        "End-to-end:           {:.3}s",
        solved_at.duration_since(started).as_secs_f64()
    );
    println!("Objective:            {:.3}", solution.summary().total_cost);
    println!("Period results:       {}", solution.periods().len());
    println!(
        "Period 0 withdrawals: {:.3}",
        solution.periods()[0]
            .bus_results()
            .iter()
            .map(|bus| bus.withdrawals_mw)
            .sum::<f64>()
    );

    Ok(())
}

fn parse_case(raw: &str) -> anyhow::Result<ActivsgCase> {
    match raw.to_ascii_lowercase().as_str() {
        "2000" | "activsg2000" => Ok(ActivsgCase::Activsg2000),
        "10k" | "10000" | "activsg10k" => Ok(ActivsgCase::Activsg10k),
        _ => Err(anyhow!(
            "unknown ACTIVSg case `{raw}`; expected `2000` or `10k`"
        )),
    }
}

fn parse_periods(raw: &str) -> anyhow::Result<usize> {
    let periods = raw
        .parse::<usize>()
        .with_context(|| format!("parse periods from `{raw}`"))?;
    if periods == 0 {
        return Err(anyhow!("periods must be >= 1"));
    }
    Ok(periods)
}

fn default_case_path(case: ActivsgCase) -> PathBuf {
    match case {
        ActivsgCase::Activsg2000 => {
            PathBuf::from("examples/cases/case_ACTIVSg2000/case_ACTIVSg2000.surge.json.zst")
        }
        ActivsgCase::Activsg10k => {
            PathBuf::from("examples/cases/case_ACTIVSg10k/case_ACTIVSg10k.surge.json.zst")
        }
    }
}

fn default_time_series_root() -> PathBuf {
    PathBuf::from("research/test-cases/data/ACTIVSg_Time_Series")
}

fn override_with_linear_smoke_costs(network: &mut Network) {
    for (index, generator) in network.generators.iter_mut().enumerate() {
        if !matches!(generator.cost, Some(CostCurve::Polynomial { .. })) {
            generator.cost = Some(CostCurve::Polynomial {
                startup: 0.0,
                shutdown: 0.0,
                coeffs: vec![10.0 + index as f64 * 1e-3, 0.0],
            });
        }
    }
}

fn ensure_missing_costs(network: &mut Network) {
    for (index, generator) in network.generators.iter_mut().enumerate() {
        if generator.cost.is_none() {
            generator.cost = Some(CostCurve::Polynomial {
                startup: 0.0,
                shutdown: 0.0,
                coeffs: vec![10.0 + index as f64 * 1e-3, 0.0],
            });
        }
    }
}
