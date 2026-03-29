// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Basic AC power flow example using the bundled IEEE 118 native case.
//!
//! Run with:
//!   cargo run --release --example basic_power_flow -p surge-ac

use std::fs;
use std::io::Cursor;
use std::path::Path;

use surge_network::Network;
use surge_solution::SolveStatus;

fn load_native_case(path: &Path) -> anyhow::Result<Network> {
    let bytes = fs::read(path)?;
    let json = zstd::stream::decode_all(Cursor::new(bytes))?;
    let document: serde_json::Value = serde_json::from_slice(&json)?;
    let mut network: Network =
        serde_json::from_value(document.get("network").cloned().unwrap_or(document))?;
    network.canonicalize_runtime_identities();
    Ok(network)
}

fn main() -> anyhow::Result<()> {
    let case = Path::new("examples/cases/ieee118/case118.surge.json.zst");
    let net = load_native_case(case)?;
    println!(
        "Loaded: {} buses, {} branches",
        net.buses.len(),
        net.branches.len()
    );

    let ac = surge_ac::solve_ac_pf(&net, &surge_ac::AcPfOptions::default())?;
    println!(
        "\nAC power flow:\n  status = {:?}\n  iterations = {}\n  max mismatch = {:.2e} pu",
        ac.status, ac.iterations, ac.max_mismatch,
    );

    if ac.status == SolveStatus::Converged {
        let mut vm: Vec<(usize, f64)> = ac
            .voltage_magnitude_pu
            .iter()
            .copied()
            .enumerate()
            .collect();
        vm.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        println!("\nLowest voltage buses:");
        for (i, v) in vm.iter().take(5) {
            println!("  bus {} (ext {}): {:.4} pu", i, net.buses[*i].number, v);
        }
    }

    Ok(())
}
