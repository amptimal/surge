// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Power-flow routing helpers for contingency analysis.

use std::collections::HashMap;

use surge_ac::{AcPfOptions, solve_ac_pf, solve_ac_pf_kernel};
use surge_hvdc::{HvdcOptions, HvdcStationSolution, solve_hvdc};
use surge_network::Network;
use surge_network::network::{BusType, Load};
use surge_solution::{PfSolution, SolveStatus};

pub(crate) fn network_has_hvdc_assets(network: &Network) -> bool {
    let has_point_to_point = network.hvdc.links.iter().any(|link| !link.is_blocked());
    let has_explicit_dc = network.hvdc.dc_grids.iter().any(|grid| {
        !grid.is_empty()
            && (grid
                .converters
                .iter()
                .any(|converter| converter.is_in_service())
                || grid.branches.iter().any(|branch| branch.status))
    });
    has_point_to_point || has_explicit_dc
}

pub(crate) fn solve_network_pf_exact(
    network: &Network,
    acpf_options: &AcPfOptions,
) -> Result<PfSolution, String> {
    if !network_has_hvdc_assets(network) {
        return solve_ac_pf_kernel(network, acpf_options).map_err(|error| error.to_string());
    }

    let hvdc_options = HvdcOptions {
        ac_tol: acpf_options.tolerance,
        max_ac_iter: acpf_options.max_iterations,
        flat_start: acpf_options.flat_start,
        ..Default::default()
    };

    let hvdc_solution = solve_hvdc(network, &hvdc_options)
        .map_err(|error| format!("HVDC solve failed: {error}"))?;
    let augmented = build_hvdc_augmented_network(network, &hvdc_solution.stations);
    solve_ac_pf(&augmented, acpf_options)
        .map_err(|error| format!("AC solve after HVDC convergence failed: {error}"))
}

pub(crate) fn solve_network_pf_with_fallback(
    network: &Network,
    acpf_options: &AcPfOptions,
) -> Result<PfSolution, String> {
    let first = solve_network_pf_exact(network, acpf_options);
    match first {
        Ok(solution) if solution.status == SolveStatus::Converged => Ok(solution),
        first_result => {
            if acpf_options.flat_start {
                Err(describe_solve_failure(first_result))
            } else {
                let flat_options = AcPfOptions {
                    flat_start: true,
                    ..acpf_options.clone()
                };
                match solve_network_pf_exact(network, &flat_options) {
                    Ok(solution) if solution.status == SolveStatus::Converged => Ok(solution),
                    _ => Err(describe_solve_failure(first_result)),
                }
            }
        }
    }
}

fn describe_solve_failure(result: Result<PfSolution, String>) -> String {
    match result {
        Ok(solution) => format!(
            "power flow did not converge (max_mismatch={:.2e} after {} iters)",
            solution.max_mismatch, solution.iterations
        ),
        Err(error) => error,
    }
}

fn build_hvdc_augmented_network(network: &Network, stations: &[HvdcStationSolution]) -> Network {
    let mut augmented = network.clone();
    augmented.hvdc.links.clear();
    augmented.hvdc.clear_dc_grids();

    let mut p_delta_mw: HashMap<u32, f64> = HashMap::new();
    let mut q_delta_mvar: HashMap<u32, f64> = HashMap::new();
    for station in stations {
        *p_delta_mw.entry(station.ac_bus).or_default() += station.p_ac_mw;
        *q_delta_mvar.entry(station.ac_bus).or_default() += station.q_ac_mvar;
    }

    for (&bus_number, &p_mw) in &p_delta_mw {
        let q_mvar = q_delta_mvar.get(&bus_number).copied().unwrap_or(0.0);
        let mut synthetic = Load::new(bus_number, -p_mw, -q_mvar);
        synthetic.conforming = false;
        synthetic.id = format!("__ctg_hvdc_inj_{bus_number}");
        augmented.loads.push(synthetic);
    }

    for bus in &mut augmented.buses {
        if p_delta_mw.contains_key(&bus.number) && bus.bus_type == BusType::Isolated {
            bus.bus_type = BusType::PQ;
        }
    }

    augmented
}
