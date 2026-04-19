// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Import TAMU ACTIVSg time-series data and build dispatch-ready profiles.
//!
//! Usage:
//!   cargo run -p surge-market --example activsg_time_series
//!   cargo run -p surge-market --example activsg_time_series -- 10k
//!   cargo run -p surge-market --example activsg_time_series -- 2000 /path/to/case.surge.json.zst /path/to/ACTIVSg_Time_Series

use std::path::PathBuf;

use anyhow::{Context, anyhow};
use surge_dispatch::datasets::{ActivsgCase, read_tamu_activsg_time_series};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let case = match args.next() {
        Some(raw) => parse_case(&raw)?,
        None => ActivsgCase::Activsg2000,
    };
    let case_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_case_path(case));
    let time_series_root = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_time_series_root);

    let network =
        surge_io::load(&case_path).with_context(|| format!("load {}", case_path.display()))?;
    let imported =
        read_tamu_activsg_time_series(&network, &time_series_root, case, &Default::default())?;

    println!("Case:              {:?}", imported.case);
    println!("Case path:         {}", case_path.display());
    println!("Time-series root:  {}", time_series_root.display());
    println!("Periods:           {}", imported.periods());
    println!(
        "Range:             {} -> {}",
        imported.report.start_timestamp, imported.report.end_timestamp
    );
    println!("Load buses:        {}", imported.report.load_buses);
    println!(
        "Renewable profiles: {}",
        imported.renewable_profiles.profiles.len()
    );
    println!(
        "Direct renewable generators: {}",
        imported.report.direct_renewable_generators
    );
    println!(
        "Solar buses aggregated:      {}",
        imported.report.solar_buses_aggregated
    );
    println!(
        "Solar generators profiled:   {}",
        imported.report.solar_generators_profiled
    );
    println!(
        "Generator pmax overrides:    {}",
        imported.report.generator_pmax_overrides
    );
    println!(
        "Inserted renewable timestamps: {}",
        imported.report.inserted_renewable_timestamps.len()
    );
    if !imported.report.dropped_solar_buses.is_empty() {
        println!(
            "Dropped solar buses: {}",
            imported
                .report
                .dropped_solar_buses
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let adjusted_network = imported.network_with_nameplate_overrides(&network)?;
    let ac_request = imported.ac_request(imported.periods())?;
    let dc_request = imported.dc_request(imported.periods())?;

    println!(
        "\nAC request: {} bus-load profiles, {} renewable profiles",
        ac_request.profiles().ac_bus_load.profiles.len(),
        ac_request.profiles().renewable.profiles.len()
    );
    println!(
        "DC request: {} load profiles, {} renewable profiles",
        dc_request.profiles().load.profiles.len(),
        dc_request.profiles().renewable.profiles.len()
    );
    println!(
        "Adjusted network renewable nameplates applied to {} generators",
        adjusted_network
            .generators
            .iter()
            .filter(|generator| imported
                .generator_pmax_overrides
                .contains_key(&generator.id))
            .count()
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
