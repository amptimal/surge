// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Sequential AC-DC iteration for HVDC power flow.
//!
//! Algorithm:
//! 1. Convert each HVDC link to equivalent P/Q injections at AC converter buses
//!    (loads with negative Pd = generation, or generators with fixed output).
//! 2. Run AC power flow on the modified network.
//! 3. From AC results (V, θ), update the converter operating points.
//! 4. Compute updated P/Q injections and check convergence.
//! 5. Repeat until max(|ΔP|, |ΔQ|) < tolerance.

use std::collections::HashMap;

use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_network::Network;
use surge_network::network::{BusType, Load};
use tracing::{debug, info, warn};

use crate::error::HvdcError;
use crate::model::lcc::{compute_lcc_operating_point, lcc_converter_results};
use crate::model::link::HvdcLink;
use crate::model::vsc::vsc_converter_results;
use crate::options::HvdcOptions;
use crate::result::{HvdcMethod, HvdcSolution, HvdcStationSolution};

/// Solve HVDC power flow using sequential AC-DC iteration.
///
/// Each HVDC link produces two [`HvdcStationSolution`]s in the solution:
/// `[rectifier, inverter]` pairs, ordered by link.
pub fn solve_sequential(
    network: &Network,
    links: &[HvdcLink],
    opts: &HvdcOptions,
) -> Result<HvdcSolution, HvdcError> {
    if links.is_empty() {
        info!(
            links = 0,
            "Sequential AC-DC solve: no links, returning trivial solution"
        );
        return Ok(HvdcSolution {
            stations: Vec::new(),
            dc_buses: Vec::new(),
            total_converter_loss_mw: 0.0,
            total_dc_network_loss_mw: 0.0,
            total_loss_mw: 0.0,
            iterations: 0,
            converged: true,
            method: HvdcMethod::Sequential,
        });
    }

    info!(
        links = links.len(),
        max_iter = opts.max_iter,
        tol = opts.tol,
        "Sequential AC-DC solve starting"
    );

    let base_mva = network.base_mva;
    let bus_map = network.bus_index_map();

    // Validate that all converter buses exist in the network.
    for link in links {
        if !bus_map.contains_key(&link.from_bus()) {
            return Err(HvdcError::BusNotFound(link.from_bus()));
        }
        if !bus_map.contains_key(&link.to_bus()) {
            return Err(HvdcError::BusNotFound(link.to_bus()));
        }
    }

    // Compute initial converter results (use unit voltages for first estimate).
    // Each link produces [rectifier, inverter].
    let mut station_results: Vec<HvdcStationSolution> = links
        .iter()
        .flat_map(|link| initial_converter_results(link, base_mva))
        .collect();

    let acpf_opts = AcPfOptions {
        tolerance: opts.ac_tol,
        max_iterations: opts.max_ac_iter,
        flat_start: opts.flat_start,
        ..AcPfOptions::default()
    };

    let mut iterations = 0u32;
    let mut outer_converged = false;
    let mut final_max_delta = 0.0_f64;

    for _outer in 0..opts.max_iter {
        iterations += 1;

        // Build augmented network with HVDC injections.
        let aug_net = build_augmented_network(network, &station_results);

        // Run AC power flow on the augmented network.
        let pf_result =
            solve_ac_pf(&aug_net, &acpf_opts).map_err(|e| HvdcError::AcPfFailed(e.to_string()))?;

        // Extract AC bus voltages.
        let vm: HashMap<u32, f64> = aug_net
            .buses
            .iter()
            .zip(pf_result.voltage_magnitude_pu.iter())
            .map(|(b, &v)| (b.number, v))
            .collect();

        // Update converter results from AC solution.
        let new_results: Vec<HvdcStationSolution> = links
            .iter()
            .flat_map(|link| {
                let v_from = vm.get(&link.from_bus()).copied().unwrap_or(1.0);
                let v_to = vm.get(&link.to_bus()).copied().unwrap_or(1.0);
                match link {
                    HvdcLink::Lcc(p) => {
                        let op = compute_lcc_operating_point(p, v_from, v_to, base_mva);
                        lcc_converter_results(p, &op).to_vec()
                    }
                    HvdcLink::Vsc(p) => vsc_converter_results(p, v_from, v_to, base_mva).to_vec(),
                }
            })
            .collect();

        // Check convergence: max change in P and Q injections across all stations.
        let max_delta = station_results
            .iter()
            .zip(new_results.iter())
            .map(|(old, new)| {
                let dp = (old.p_ac_mw - new.p_ac_mw).abs();
                let dq = (old.q_ac_mvar - new.q_ac_mvar).abs();
                dp.max(dq)
            })
            .fold(0.0_f64, f64::max);

        debug!(
            iteration = iterations,
            max_delta_mw = max_delta,
            "HVDC AC-DC outer iteration"
        );

        final_max_delta = max_delta;

        station_results = new_results;

        if max_delta < opts.tol {
            outer_converged = true;
            break;
        }
    }

    let total_converter_loss_mw = station_results
        .iter()
        .map(|r| r.converter_loss_mw)
        .sum::<f64>();
    let total_loss_mw = total_converter_loss_mw;

    if !outer_converged {
        warn!(
            iterations = iterations,
            max_iter = opts.max_iter,
            tol = opts.tol,
            "Sequential AC-DC solve did not converge"
        );
        return Err(HvdcError::NotConverged {
            iterations,
            max_delta: final_max_delta,
        });
    } else {
        info!(
            iterations = iterations,
            total_loss_mw = total_loss_mw,
            converged = outer_converged,
            "Sequential AC-DC solve complete"
        );
    }

    Ok(HvdcSolution {
        stations: station_results,
        dc_buses: Vec::new(), // no explicit DC modeling in sequential
        total_converter_loss_mw,
        total_dc_network_loss_mw: 0.0,
        total_loss_mw,
        iterations,
        converged: outer_converged,
        method: HvdcMethod::Sequential,
    })
}

/// Compute initial converter results using unity AC voltages.
fn initial_converter_results(link: &HvdcLink, base_mva: f64) -> Vec<HvdcStationSolution> {
    match link {
        HvdcLink::Lcc(p) => {
            let op = compute_lcc_operating_point(p, 1.0, 1.0, base_mva);
            lcc_converter_results(p, &op).to_vec()
        }
        HvdcLink::Vsc(p) => vsc_converter_results(p, 1.0, 1.0, base_mva).to_vec(),
    }
}

/// Build an augmented network that includes HVDC P/Q injections as loads.
fn build_augmented_network(network: &Network, converters: &[HvdcStationSolution]) -> Network {
    let mut aug = network.clone();
    // The sequential outer loop has already converted point-to-point HVDC
    // links into equivalent AC injections for this iteration. The inner AC
    // solve must therefore operate on a pure AC view of the augmented
    // network so it does not apply the embedded HVDC preprocessing a second
    // time.
    aug.hvdc.links.clear();
    aug.hvdc.clear_dc_grids();

    // Accumulate HVDC P/Q deltas per bus (in MW/MVAR).
    let mut p_delta: HashMap<u32, f64> = HashMap::new();
    let mut q_delta: HashMap<u32, f64> = HashMap::new();

    for res in converters {
        *p_delta.entry(res.ac_bus).or_default() += res.p_ac_mw;
        *q_delta.entry(res.ac_bus).or_default() += res.q_ac_mvar;
    }

    // Apply deltas as loads (a positive injection is modelled as negative load).
    for (bus_num, &p_mw) in &p_delta {
        let q_mvar = q_delta.get(bus_num).copied().unwrap_or(0.0);
        aug.loads.push(Load::new(*bus_num, -p_mw, -q_mvar));
    }

    // Convert isolated converter buses to PQ so injections are visible.
    let hvdc_buses: std::collections::HashSet<u32> = p_delta.keys().copied().collect();
    for bus in aug.buses.iter_mut() {
        if hvdc_buses.contains(&bus.number) && bus.bus_type == BusType::Isolated {
            bus.bus_type = BusType::PQ;
        }
    }

    aug
}
