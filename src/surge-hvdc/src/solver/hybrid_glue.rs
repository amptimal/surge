// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Hybrid AC/DC solver orchestration for mixed LCC+VSC MTDC networks.
//!
//! Provides the glue between the AC power flow solver and the hybrid MTDC
//! Newton-Raphson solver, including network construction from canonical
//! `surge_network::Network` data and result conversion.

use std::collections::HashMap;

use surge_ac::{AcPfOptions, solve_ac_pf_kernel};
use surge_network::Network;
use surge_network::network::Load;

use crate::bridge;
use crate::dc_network;
use crate::error::HvdcError;
use crate::options::HvdcOptions;
use crate::result::{self, HvdcSolution};
use crate::single_explicit_dc_grid;
use crate::solver::hybrid_mtdc;

// ── Public entry point ───────────────────────────────────────────────────────

/// Solve hybrid AC/DC power flow for a network with mixed LCC+VSC converters.
///
/// Orchestrates an outer iteration between the AC NR solver and the hybrid
/// MTDC DC-side Newton-Raphson solver until the converter P/Q injections
/// stabilize.
pub(crate) fn solve_hybrid_ac_dc(
    network: &Network,
    options: &HvdcOptions,
) -> Result<HvdcSolution, HvdcError> {
    let hybrid_net = build_hybrid_mtdc_from_network(network)?;
    let acpf_opts = AcPfOptions {
        tolerance: options.ac_tol,
        max_iterations: options.max_ac_iter,
        flat_start: options.flat_start,
        ..AcPfOptions::default()
    };

    let flat_ac = flat_ac_voltages_by_bus_number(network);
    let mut final_hybrid =
        hybrid_mtdc::solve_hybrid_mtdc(&hybrid_net, &flat_ac, options.max_iter, options.tol)?;
    let mut previous_injections = hybrid_injections_from_result(&final_hybrid);
    let mut outer_converged = false;
    let mut outer_iterations = 0u32;
    let mut final_max_delta = 0.0_f64;
    let mut warm_vm: Vec<f64> = Vec::new();
    let mut warm_va: Vec<f64> = Vec::new();

    for outer in 0..options.max_iter {
        outer_iterations = outer + 1;

        let mut working_network = network.clone();
        apply_hybrid_injections(&mut working_network, &previous_injections);

        let inner_opts = if !warm_vm.is_empty() && warm_vm.len() == working_network.buses.len() {
            for (i, bus) in working_network.buses.iter_mut().enumerate() {
                bus.voltage_magnitude_pu = warm_vm[i];
                bus.voltage_angle_rad = warm_va[i];
            }
            let mut opts = acpf_opts.clone();
            opts.flat_start = false;
            opts
        } else {
            acpf_opts.clone()
        };

        let ac_solution = solve_ac_pf_kernel(&working_network, &inner_opts)
            .map_err(|err| HvdcError::AcPfFailed(err.to_string()))?;

        warm_vm = ac_solution.voltage_magnitude_pu.clone();
        warm_va = ac_solution.voltage_angle_rad.clone();

        let ac_voltages = build_ac_voltage_array_by_bus_number(
            &ac_solution.voltage_magnitude_pu,
            &ac_solution.voltage_angle_rad,
            &working_network,
        );
        let new_hybrid = hybrid_mtdc::solve_hybrid_mtdc(
            &hybrid_net,
            &ac_voltages,
            options.max_iter,
            options.tol,
        )?;
        let new_injections = hybrid_injections_from_result(&new_hybrid);
        let max_delta = max_hybrid_injection_delta(&previous_injections, &new_injections);
        final_max_delta = max_delta;

        final_hybrid = new_hybrid;
        previous_injections = new_injections;

        if max_delta < options.tol {
            outer_converged = true;
            break;
        }
    }

    if !outer_converged {
        return Err(HvdcError::NotConverged {
            iterations: outer_iterations,
            max_delta: final_max_delta,
        });
    }

    let mut solution = hybrid_mtdc_to_hvdc_solution(network, &final_hybrid);
    solution.converged = solution.converged && outer_converged;
    solution.iterations = outer_iterations;
    Ok(solution)
}

// ── Network builder ──────────────────────────────────────────────────────────

/// Build a hybrid MTDC network from the explicit DC topology in [`Network`].
///
/// Maps `Network.dc_buses` + `Network.dc_branches` + `Network.dc_converters`
/// to the hybrid MTDC solver's data structures, supporting both LCC and VSC.
pub(crate) fn build_hybrid_mtdc_from_network(
    network: &Network,
) -> Result<hybrid_mtdc::HybridMtdcNetwork, HvdcError> {
    let base_mva = network.base_mva;
    let ac_bus_map = network.bus_index_map();
    let dc_grid = single_explicit_dc_grid(network)?;
    let n_dc_buses = dc_grid.buses.len();

    // Map DC bus IDs -> 0-indexed internal indices.
    let mut dc_bus_map: HashMap<u32, usize> = HashMap::new();
    for (i, db) in dc_grid.buses.iter().enumerate() {
        dc_bus_map.insert(db.bus_id, i);
    }

    // First pass over converters to find the slack bus index.
    let mut slack_dc_bus = 0usize;
    let mut slack_count = 0u32;
    for conv in &dc_grid.converters {
        if let Some(vsc) = conv.as_vsc() {
            if vsc.status && vsc.control_type_dc == 2 {
                if let Some(&idx) = dc_bus_map.get(&vsc.dc_bus) {
                    slack_dc_bus = idx;
                    slack_count += 1;
                }
            }
        }
    }
    if slack_count == 0 {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit hybrid MTDC topology requires exactly one VSC converter with DC-voltage control"
                .to_string(),
        ));
    }
    if slack_count > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "explicit hybrid MTDC topology has multiple DC-voltage slack converters".to_string(),
        ));
    }

    let mut hybrid = hybrid_mtdc::HybridMtdcNetwork::new(base_mva, n_dc_buses, slack_dc_bus);

    // Populate per-bus data.
    for (i, db) in dc_grid.buses.iter().enumerate() {
        let z_base = dc_network::dc_bus_z_base(base_mva, db.base_kv_dc)?;
        hybrid.dc_network.v_dc[i] = if db.v_dc_pu > 0.0 { db.v_dc_pu } else { 1.0 };
        if db.g_shunt_siemens > 0.0 {
            hybrid.dc_network.g_shunt_pu[i] = db.g_shunt_siemens * z_base;
        }
        if db.r_ground_ohm > 0.0 {
            hybrid.dc_network.g_ground_pu[i] = z_base / db.r_ground_ohm;
        }
    }

    // DC branches.
    for br in &dc_grid.branches {
        if !br.status {
            continue;
        }
        let from = dc_bus_map.get(&br.from_bus).copied().ok_or_else(|| {
            HvdcError::InvalidLink(format!("DC branch from_bus {} not found", br.from_bus))
        })?;
        let to = dc_bus_map.get(&br.to_bus).copied().ok_or_else(|| {
            HvdcError::InvalidLink(format!("DC branch to_bus {} not found", br.to_bus))
        })?;
        let z_base = dc_network::dc_branch_z_base(
            base_mva,
            dc_grid.buses[from].base_kv_dc,
            dc_grid.buses[to].base_kv_dc,
        )?;
        let r_pu = if br.r_ohm > 1e-12 {
            br.r_ohm / z_base
        } else {
            1e-6
        };
        hybrid.dc_network.add_branch(dc_network::DcBranch {
            from_dc_bus: from,
            to_dc_bus: to,
            r_dc_pu: r_pu,
            i_max_pu: 0.0,
        });
    }

    // Converter stations.
    for conv in &dc_grid.converters {
        if !conv.is_in_service() {
            continue;
        }
        if !ac_bus_map.contains_key(&conv.ac_bus()) {
            return Err(HvdcError::BusNotFound(conv.ac_bus()));
        }
        let dc_idx = dc_bus_map.get(&conv.dc_bus()).copied().ok_or_else(|| {
            HvdcError::InvalidLink(format!(
                "Converter dc_bus {} not found in dc_buses",
                conv.dc_bus()
            ))
        })?;

        match conv {
            surge_network::network::DcConverter::Lcc(converter) => {
                let mut lcc_conv = hybrid_mtdc::LccConverter::new(
                    converter.ac_bus,
                    dc_idx,
                    converter.scheduled_setpoint.abs(),
                );
                lcc_conv.p_setpoint_mw = converter.scheduled_setpoint;
                lcc_conv.x_commutation_pu = bridge::lcc_commutation_reactance_pu(
                    converter.commutation_reactance_ohm,
                    converter.n_bridges,
                    converter.base_voltage_kv,
                    base_mva,
                )
                .ok_or_else(|| {
                    HvdcError::UnsupportedConfiguration(format!(
                        "LCC converter at AC bus {} requires positive base_voltage_kv, positive base_mva, and n_bridges >= 1",
                        converter.ac_bus
                    ))
                })?;
                lcc_conv.alpha_min_deg = converter.alpha_min_deg;
                lcc_conv.alpha_max_deg = converter.alpha_max_deg;
                lcc_conv.gamma_min_deg = converter.gamma_min_deg;
                hybrid.lcc_converters.push(lcc_conv);
            }
            surge_network::network::DcConverter::Vsc(converter) => {
                let is_slack = converter.control_type_dc == 2;
                let mut vsc_conv = hybrid_mtdc::HybridVscConverter::new(
                    converter.ac_bus,
                    dc_idx,
                    converter.power_dc_setpoint_mw,
                );
                vsc_conv.q_setpoint_mvar = converter.reactive_power_mvar;
                vsc_conv.q_max_mvar = converter.reactive_power_ac_max_mvar;
                vsc_conv.q_min_mvar = converter.reactive_power_ac_min_mvar;
                vsc_conv.is_dc_slack = is_slack;
                vsc_conv.v_dc_setpoint = converter.voltage_dc_setpoint_pu.max(0.05);
                vsc_conv.loss_constant_mw = converter.loss_constant_mw / base_mva;
                vsc_conv.loss_linear = converter.loss_linear;
                vsc_conv.loss_quadratic_rectifier = converter.loss_quadratic_rectifier;
                vsc_conv.loss_quadratic_inverter = converter.loss_quadratic_inverter;
                hybrid.vsc_converters.push(vsc_conv);

                if is_slack {
                    hybrid.dc_network.v_dc_slack = converter.voltage_dc_setpoint_pu.max(0.05);
                    hybrid.dc_network.v_dc[dc_idx] = hybrid.dc_network.v_dc_slack;
                }
            }
        }
    }

    Ok(hybrid)
}

// ── Result conversion ────────────────────────────────────────────────────────

/// Convert a hybrid MTDC result to the canonical [`HvdcSolution`].
pub(crate) fn hybrid_mtdc_to_hvdc_solution(
    network: &Network,
    result: &hybrid_mtdc::HybridMtdcResult,
) -> HvdcSolution {
    let mut stations = Vec::new();
    let dc_bus_ids: Vec<u32> = network.hvdc.dc_buses().map(|bus| bus.bus_id).collect();
    let lcc_meta = explicit_converter_metadata(network, true);
    let vsc_meta = explicit_converter_metadata(network, false);

    for (index, lcc_r) in result.lcc_results.iter().enumerate() {
        let meta = lcc_meta.get(index);
        let losses = (lcc_r.p_ac_mw.abs() - lcc_r.p_dc_mw.abs()).abs();
        stations.push(result::HvdcStationSolution {
            name: meta.map(|entry| entry.converter_id.clone()),
            technology: result::HvdcTechnology::Lcc,
            ac_bus: lcc_r.bus_ac,
            dc_bus: meta.map(|entry| entry.dc_bus),
            p_ac_mw: lcc_r.p_ac_mw,
            q_ac_mvar: lcc_r.q_ac_mvar,
            p_dc_mw: lcc_r.p_dc_mw,
            v_dc_pu: lcc_r.v_dc_pu,
            converter_loss_mw: losses,
            lcc_detail: Some(result::HvdcLccDetail {
                alpha_deg: lcc_r.alpha_deg,
                gamma_deg: lcc_r.gamma_deg,
                i_dc_pu: lcc_r.i_dc_pu,
                power_factor: lcc_r.power_factor,
            }),
            converged: result.converged,
        });
    }

    for (index, vsc_r) in result.vsc_results.iter().enumerate() {
        let meta = vsc_meta.get(index);
        stations.push(result::HvdcStationSolution {
            name: meta.map(|entry| entry.converter_id.clone()),
            technology: result::HvdcTechnology::Vsc,
            ac_bus: vsc_r.bus_ac,
            dc_bus: meta.map(|entry| entry.dc_bus),
            p_ac_mw: vsc_r.p_ac_mw,
            q_ac_mvar: vsc_r.q_ac_mvar,
            p_dc_mw: vsc_r.p_dc_mw,
            v_dc_pu: vsc_r.v_dc_pu,
            converter_loss_mw: vsc_r.losses_mw,
            lcc_detail: None,
            converged: result.converged,
        });
    }

    let total_converter_loss_mw: f64 = stations.iter().map(|c| c.converter_loss_mw).sum::<f64>();
    let total_dc_network_loss_mw = result.total_dc_loss_mw;
    let total_loss_mw = total_converter_loss_mw + total_dc_network_loss_mw;
    let dc_buses = result
        .dc_voltages_pu
        .iter()
        .enumerate()
        .map(|(idx, &voltage_pu)| result::HvdcDcBusSolution {
            dc_bus: dc_bus_ids.get(idx).copied().unwrap_or(idx as u32),
            voltage_pu,
        })
        .collect();

    result::HvdcSolution {
        stations,
        dc_buses,
        total_converter_loss_mw,
        total_dc_network_loss_mw,
        total_loss_mw,
        iterations: result.iterations,
        converged: result.converged,
        method: result::HvdcMethod::Hybrid,
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct HybridInjection {
    ac_bus: u32,
    p_mw: f64,
    q_mvar: f64,
}

#[derive(Clone, Debug)]
struct ExplicitConverterMetadata {
    converter_id: String,
    dc_bus: u32,
}

fn explicit_converter_metadata(network: &Network, lcc: bool) -> Vec<ExplicitConverterMetadata> {
    let mut meta = Vec::new();
    for grid in &network.hvdc.dc_grids {
        for (index, converter) in grid.converters.iter().enumerate() {
            if !converter.is_in_service() || converter.is_lcc() != lcc {
                continue;
            }
            let converter_id = {
                let trimmed = converter.id().trim();
                if trimmed.is_empty() {
                    format!("dc_grid_{}_converter_{}", grid.id, index + 1)
                } else {
                    trimmed.to_string()
                }
            };
            meta.push(ExplicitConverterMetadata {
                converter_id,
                dc_bus: converter.dc_bus(),
            });
        }
    }
    meta
}

fn hybrid_injections_from_result(result: &hybrid_mtdc::HybridMtdcResult) -> Vec<HybridInjection> {
    let mut injections = Vec::with_capacity(result.lcc_results.len() + result.vsc_results.len());
    for lcc in &result.lcc_results {
        injections.push(HybridInjection {
            ac_bus: lcc.bus_ac,
            p_mw: lcc.p_ac_mw,
            q_mvar: lcc.q_ac_mvar,
        });
    }
    for vsc in &result.vsc_results {
        injections.push(HybridInjection {
            ac_bus: vsc.bus_ac,
            p_mw: vsc.p_ac_mw,
            q_mvar: vsc.q_ac_mvar,
        });
    }
    injections
}

fn apply_hybrid_injections(network: &mut Network, injections: &[HybridInjection]) {
    if injections.is_empty() {
        return;
    }
    for injection in injections {
        let mut load = Load::new(injection.ac_bus, -injection.p_mw, -injection.q_mvar);
        load.id = format!("__hvdc_hybrid_inj_{}", injection.ac_bus);
        network.loads.push(load);
    }
}

fn max_hybrid_injection_delta(previous: &[HybridInjection], next: &[HybridInjection]) -> f64 {
    if previous.len() != next.len() {
        return f64::INFINITY;
    }
    previous
        .iter()
        .zip(next.iter())
        .map(|(old, new)| {
            let dp = (old.p_mw - new.p_mw).abs();
            let dq = (old.q_mvar - new.q_mvar).abs();
            dp.max(dq)
        })
        .fold(0.0_f64, f64::max)
}

fn flat_ac_voltages_by_bus_number(network: &Network) -> Vec<num_complex::Complex64> {
    let max_bus = network
        .buses
        .iter()
        .map(|bus| bus.number as usize)
        .max()
        .unwrap_or(0);
    vec![num_complex::Complex64::new(1.0, 0.0); max_bus + 1]
}

fn build_ac_voltage_array_by_bus_number(
    vm: &[f64],
    va: &[f64],
    network: &Network,
) -> Vec<num_complex::Complex64> {
    let max_bus = network
        .buses
        .iter()
        .map(|bus| bus.number as usize)
        .max()
        .unwrap_or(0);
    let mut voltages = vec![num_complex::Complex64::new(1.0, 0.0); max_bus + 1];
    for ((bus, &mag), &ang) in network.buses.iter().zip(vm.iter()).zip(va.iter()) {
        let idx = bus.number as usize;
        if idx < voltages.len() {
            voltages[idx] = num_complex::Complex64::from_polar(mag, ang);
        }
    }
    voltages
}
