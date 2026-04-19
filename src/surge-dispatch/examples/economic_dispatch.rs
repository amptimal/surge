// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Economic dispatch example.
//!
//! Loads the bundled IEEE 118-bus case, runs a single-period DC SCED, and
//! prints dispatch results and LMPs.
//!
//! Run with:
//!   cargo run --release --example economic_dispatch -p surge-dispatch

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let case = Path::new("examples/cases/ieee118/case118.surge.json");
    let net = surge_io::load(case)?;
    println!(
        "Loaded: {} buses, {} generators, {} branches",
        net.buses.len(),
        net.generators.len(),
        net.branches.len(),
    );

    // Build a single-period DC dispatch request (SCED).
    let model = surge_dispatch::DispatchModel::prepare(&net)?;
    let request = surge_dispatch::DispatchRequest::builder()
        .dc()
        .period_by_period()
        .all_committed()
        .timeline(surge_dispatch::DispatchTimeline::hourly(1))
        .build();

    let solution = model.solve(&request)?;
    let period = &solution.periods()[0];

    println!("\n--- Single-Period DC SCED ---");
    println!("Total cost:  {:.2} $/hr", period.total_cost());

    // Print top 5 generator/storage resources by dispatch.
    println!("\nTop dispatched generators:");
    println!("{:>16}  {:>8}  {:>10}", "Resource", "Bus", "Pg (MW)");
    let mut dispatch: Vec<_> = period
        .resource_results()
        .iter()
        .filter(|resource| resource.power_mw > 0.0)
        .map(|resource| {
            (
                resource.resource_id.as_str(),
                resource.bus_number.unwrap_or_default(),
                resource.power_mw,
            )
        })
        .collect();
    dispatch.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    for (resource_id, bus, pg) in dispatch.iter().take(5) {
        println!("{:>16}  {:>8}  {:>10.1}", resource_id, bus, pg);
    }

    // Print LMP statistics.
    if !period.bus_results().is_empty() {
        let min_lmp = period
            .bus_results()
            .iter()
            .map(|bus| bus.lmp)
            .fold(f64::INFINITY, f64::min);
        let max_lmp = period
            .bus_results()
            .iter()
            .map(|bus| bus.lmp)
            .fold(f64::NEG_INFINITY, f64::max);
        let avg_lmp = period.bus_results().iter().map(|bus| bus.lmp).sum::<f64>()
            / period.bus_results().len() as f64;
        println!("\nLMP ($/MWh): min={min_lmp:.2}  avg={avg_lmp:.2}  max={max_lmp:.2}");
    }

    Ok(())
}
