// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! DC optimal power flow example.
//!
//! Loads the bundled IEEE 118-bus case, solves a DC-OPF, and prints dispatch,
//! LMP statistics, and congested branches.
//!
//! Run with:
//!   cargo run --release --example dc_opf -p surge-opf

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let case = Path::new("examples/cases/ieee118/case118.surge.json.zst");
    let net = surge_io::load(case)?;

    let spec = surge_opf::DcOpfOptions {
        enforce_thermal_limits: true,
        ..Default::default()
    };
    let result = surge_opf::solve_dc_opf(&net, &spec)?;
    let sol = &result.opf;

    println!("DC-OPF solved in {:.1} ms", sol.solve_time_secs * 1e3);
    println!("Total cost: ${:.2}/hr", sol.total_cost);
    println!("Total generation: {:.1} MW", sol.total_generation_mw);
    println!("Total load: {:.1} MW", sol.total_load_mw);

    // LMP statistics.
    let lmp_min = sol
        .pricing
        .lmp
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let lmp_max = sol
        .pricing
        .lmp
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let lmp_avg = sol.pricing.lmp.iter().sum::<f64>() / sol.pricing.lmp.len() as f64;
    println!(
        "\nLMP ($/MWh): min={:.2}, avg={:.2}, max={:.2}",
        lmp_min, lmp_avg, lmp_max
    );

    // Show congested branches (non-zero shadow price).
    let congested: Vec<_> = sol
        .branches
        .branch_shadow_prices
        .iter()
        .enumerate()
        .filter(|(_, sp)| sp.abs() > 1e-6)
        .collect();
    println!("\nCongested branches: {}", congested.len());
    for (i, sp) in congested.iter().take(10) {
        let br = &net.branches[*i];
        println!(
            "  {}-{} ckt {}: shadow price = ${:.2}/MWh",
            br.from_bus, br.to_bus, br.circuit, sp
        );
    }

    Ok(())
}
