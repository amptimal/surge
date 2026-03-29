// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! N-1 contingency screening example.
//!
//! Loads the bundled IEEE 118-bus case, runs N-1 branch contingency analysis
//! with LODF screening, and reports violations.
//!
//! Run with:
//!   cargo run --release --example contingency_screening -p surge-contingency

use std::path::Path;

use surge_contingency::{ContingencyOptions, ScreeningMode, Violation, analyze_n1_branch};

fn main() -> anyhow::Result<()> {
    let case = Path::new("examples/cases/ieee118/case118.surge.json.zst");
    let net = surge_io::load(case)?;

    let options = ContingencyOptions {
        screening: ScreeningMode::Lodf,
        ..Default::default()
    };
    let analysis = analyze_n1_branch(&net, &options)?;

    println!(
        "N-1 branch contingency analysis:\n  contingencies evaluated: {}\n  screened out (LODF): {}\n  AC solved: {}\n  with violations: {}\n  solve time: {:.1} ms",
        analysis.summary.total_contingencies,
        analysis.summary.screened_out,
        analysis.summary.ac_solved,
        analysis.summary.with_violations,
        analysis.summary.solve_time_secs * 1e3,
    );

    // Collect the worst thermal violations across all contingencies.
    let mut worst_thermal: Vec<(String, usize, f64)> = Vec::new();
    for result in &analysis.results {
        for v in &result.violations {
            if let Violation::ThermalOverload {
                branch_idx,
                loading_pct,
                ..
            } = v
            {
                worst_thermal.push((result.label.clone(), *branch_idx, *loading_pct));
            }
        }
    }
    worst_thermal.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    if worst_thermal.is_empty() {
        println!("\nNo thermal violations found — system is N-1 secure.");
    } else {
        println!("\nWorst thermal overloads:");
        for (label, br_idx, pct) in worst_thermal.iter().take(10) {
            let br = &net.branches[*br_idx];
            println!(
                "  outage \"{label}\": branch {}-{} ckt {} loaded at {pct:.1}%",
                br.from_bus, br.to_bus, br.circuit
            );
        }
    }

    Ok(())
}
