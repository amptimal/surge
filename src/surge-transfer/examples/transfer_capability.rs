// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Transfer capability analysis example.
//!
//! Loads the IEEE 30-bus case (which has thermal ratings on every branch),
//! computes NERC ATC for a cross-area transfer path, and prints the results.
//!
//! Run with:
//!   cargo run --release --example transfer_capability -p surge-transfer

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let case = Path::new("examples/cases/case30/case30.surge.json.zst");
    let net = surge_io::load(case)?;
    println!(
        "Loaded: {} buses, {} branches",
        net.buses.len(),
        net.branches.len(),
    );

    // Transfer from area 1 (bus 1) to area 3 (bus 30).
    // This crosses two area boundaries and bottlenecks on the
    // 16 MVA corridor between buses 27-29-30.
    let monitored: Vec<usize> = (0..net.n_branches()).collect();
    let contingency: Vec<usize> = (0..net.n_branches()).collect();

    let request = surge_transfer::NercAtcRequest {
        path: surge_transfer::TransferPath::new("area1_to_area3", vec![1], vec![30]),
        options: surge_transfer::AtcOptions {
            monitored_branches: Some(monitored),
            contingency_branches: Some(contingency),
            margins: surge_transfer::AtcMargins::default(),
        },
    };

    let result = surge_transfer::compute_nerc_atc(&net, &request)?;

    println!("\n--- NERC ATC (MOD-029/MOD-030) ---");
    println!("Transfer path:     bus 1 → bus 30");
    println!("TTC:               {:.1} MW", result.ttc_mw);
    println!("TRM:               {:.1} MW", result.trm_mw);
    println!("CBM:               {:.1} MW", result.cbm_mw);
    println!("ETC:               {:.1} MW", result.etc_mw);
    println!("ATC:               {:.1} MW", result.atc_mw);
    println!("Limit cause:       {}", result.limit_cause);
    match result.limit_cause {
        surge_transfer::NercAtcLimitCause::Unconstrained => {}
        surge_transfer::NercAtcLimitCause::BasecaseThermal { monitored_branch } => {
            let br = &net.branches[monitored_branch];
            println!(
                "Limiting branch:   {} → {} (idx {})",
                br.from_bus, br.to_bus, monitored_branch
            );
        }
        surge_transfer::NercAtcLimitCause::ContingencyThermal {
            monitored_branch,
            contingency_branch,
        } => {
            let monitored = &net.branches[monitored_branch];
            let outage = &net.branches[contingency_branch];
            println!(
                "Limiting branch:   {} → {} (idx {})",
                monitored.from_bus, monitored.to_bus, monitored_branch
            );
            println!(
                "Binding contingency: {} → {} (idx {})",
                outage.from_bus, outage.to_bus, contingency_branch
            );
        }
        surge_transfer::NercAtcLimitCause::FailClosedOutage { contingency_branch } => {
            let outage = &net.branches[contingency_branch];
            println!(
                "Fail-closed outage: {} → {} (idx {})",
                outage.from_bus, outage.to_bus, contingency_branch
            );
        }
    }

    Ok(())
}
