// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF with HVDC links via joint AC-DC NLP augmentation + sequential fallback.
//!
//! **Joint NLP path** (explicit DC-network data — `network.hvdc.dc_grids`):
//! Augments the pure-AC NLP with HVDC converter and DC bus variables so that
//! HVDC power flow is co-optimized with AC generation dispatch in a single
//! Ipopt call.  Variables: `P_conv[k]`, `Q_conv[k]`, `V_dc[d]`.
//! Constraints: DC KCL at each DC bus.
//!
//! **Sequential fallback** (point-to-point `network.hvdc` links):
//! Wraps the pure-AC `solve_ac_opf` with an outer loop that iterates converter
//! P/Q injections until convergence.

use std::collections::HashMap;

use num_complex::Complex64;
use serde::{Deserialize, Serialize};
use surge_hvdc::HvdcLink;
use surge_hvdc::interop::{
    apply_dc_grid_injections, dc_grid_injections_from_voltages, links_from_network,
};
use surge_network::Network;
use surge_network::network::{BusType, Load};
use tracing::{debug, info, warn};

use super::types::{AcOpfError, AcOpfOptions, AcOpfRunContext, AcOpfRuntime};
use surge_solution::OpfSolution;

/// Maximum number of outer AC-DC iterations for the sequential approach.
const MAX_HVDC_ITERATIONS: u32 = 10;

/// Convergence tolerance for HVDC P/Q mismatch (MW / MVAr).
const HVDC_CONVERGENCE_TOL_MW: f64 = 1.0;

/// Result of AC-OPF with HVDC links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcOpfHvdcResult {
    /// The AC-OPF solution (dispatch, LMPs, voltages, etc.).
    pub opf: OpfSolution,
    /// DC power for each HVDC link (MW), in the same order as the extracted links.
    pub hvdc_p_dc_mw: Vec<f64>,
    /// Losses for each HVDC link (MW).
    pub hvdc_p_loss_mw: Vec<f64>,
    /// Number of outer AC-DC iterations taken (0 for joint NLP).
    pub hvdc_iterations: u32,
}

// ---------------------------------------------------------------------------
// Joint NLP data structures
// ---------------------------------------------------------------------------

/// HVDC data prepared for the joint AC-DC NLP.
pub(crate) struct HvdcNlpData {
    /// Number of in-service DC converters.
    pub n_conv: usize,
    /// Number of DC buses.
    pub n_dc_bus: usize,
    /// Per-converter data.
    pub converters: Vec<HvdcConverterNlp>,
    /// DC bus conductance matrix G_dc\[i\]\[j\] in per-unit.
    pub g_dc: Vec<Vec<f64>>,
    /// DC bus voltage bounds \[vdc_min, vdc_max\] in pu.
    pub vdc_bounds: Vec<(f64, f64)>,
    /// Map from DC bus index to list of converter indices at that bus.
    pub dc_bus_conv_map: Vec<Vec<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HvdcDcControlMode {
    Power,
    Voltage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HvdcAcControlMode {
    ReactivePower,
    AcVoltage,
}

/// Per-converter NLP data.
pub(crate) struct HvdcConverterNlp {
    /// AC bus internal index (into network.buses[]).
    pub ac_bus_idx: usize,
    /// DC bus internal index (0-based within the DC grid).
    pub dc_bus_idx: usize,
    /// P_conv bounds [pac_min, pac_max] in pu.
    pub p_min_pu: f64,
    pub p_max_pu: f64,
    /// Q_conv bounds [qac_min, qac_max] in pu.
    pub q_min_pu: f64,
    pub q_max_pu: f64,
    /// Constant loss coefficient in pu (loss_a_MW / base_mva).
    pub loss_a_pu: f64,
    /// Linear loss coefficient in pu (LossB_kV / kVbase).
    pub loss_linear: f64,
    /// Quadratic loss coefficient in pu (LossCrec_Ω / Zbase).
    pub loss_c: f64,
    /// Maximum converter AC-side current magnitude in pu.
    pub i_max_pu: f64,
    /// Canonical DC-side control contract for the converter.
    pub dc_control: HvdcDcControlMode,
    /// Canonical AC-side control contract for the converter.
    pub ac_control: HvdcAcControlMode,
    /// DC power setpoint in pu (for type_dc=1).
    pub p_dc_set_pu: f64,
    /// DC voltage setpoint in pu (for type_dc=2).
    pub voltage_dc_setpoint_pu: f64,
    /// AC reactive-power setpoint in pu (for PQ / reactive-power control).
    pub q_ac_set_pu: f64,
    /// AC voltage-magnitude setpoint in pu (for PV / AC-voltage control).
    pub voltage_ac_setpoint_pu: f64,
}

/// Build HVDC NLP data from the network's explicit DC-network structures.
///
/// Returns `Ok(None)` when there is no explicit DC topology to model. Any
/// malformed in-service converter/branch or unsupported explicit-HVDC feature
/// set is rejected with an error rather than being silently dropped.
pub(crate) fn build_hvdc_nlp_data(network: &Network) -> Result<Option<HvdcNlpData>, AcOpfError> {
    if !network.hvdc.has_explicit_dc_topology() {
        return Ok(None);
    }

    let base_mva = network.base_mva;
    let ac_bus_map = network.bus_index_map();
    let dc_buses: Vec<_> = network.hvdc.dc_buses().collect();
    let dc_converters: Vec<_> = network.hvdc.dc_converters().collect();
    let dc_branches: Vec<_> = network.hvdc.dc_branches().collect();
    if dc_buses.is_empty() || dc_converters.is_empty() {
        return Ok(None);
    }

    // Map DC bus numbers to 0-based indices.
    let mut dc_bus_map: HashMap<u32, usize> = HashMap::new();
    for (i, dcb) in dc_buses.iter().enumerate() {
        dc_bus_map.insert(dcb.bus_id, i);
    }
    let n_dc_bus = dc_buses.len();

    // Build converter NLP data.
    let mut converters: Vec<HvdcConverterNlp> = Vec::new();
    for conv in dc_converters.iter() {
        let Some(conv) = conv.as_vsc() else {
            return Err(AcOpfError::SolverError(
                "joint AC-OPF HVDC does not yet support explicit LCC converters".to_string(),
            ));
        };
        if !conv.status {
            continue;
        }
        let dc_control = match conv.control_type_dc {
            1 => HvdcDcControlMode::Power,
            2 => HvdcDcControlMode::Voltage,
            3 => {
                return Err(AcOpfError::SolverError(
                    "joint AC-OPF HVDC does not yet support droop-controlled explicit DC converters"
                        .to_string(),
                ));
            }
            other => {
                return Err(AcOpfError::InvalidNetwork(format!(
                    "explicit DC converter at AC bus {} has unsupported DC control type {other}",
                    conv.ac_bus
                )));
            }
        };
        if conv.droop.abs() > 1e-12 {
            return Err(AcOpfError::SolverError(
                "joint AC-OPF HVDC does not yet support droop-controlled explicit DC converters"
                    .to_string(),
            ));
        }
        let ac_control = match conv.control_type_ac {
            1 => HvdcAcControlMode::ReactivePower,
            2 => HvdcAcControlMode::AcVoltage,
            other => {
                return Err(AcOpfError::InvalidNetwork(format!(
                    "explicit DC converter at AC bus {} has unsupported AC control type {other}",
                    conv.ac_bus
                )));
            }
        };

        let bidirectional = conv.active_power_ac_min_mw < 0.0 && conv.active_power_ac_max_mw > 0.0;
        if bidirectional
            && (conv.loss_quadratic_rectifier - conv.loss_quadratic_inverter).abs() > 1e-12
        {
            return Err(AcOpfError::SolverError(
                "joint AC-OPF HVDC does not yet support bidirectional converters with asymmetric quadratic losses"
                    .to_string(),
            ));
        }

        let ac_bus_idx = *ac_bus_map.get(&conv.ac_bus).ok_or_else(|| {
            AcOpfError::InvalidNetwork(format!(
                "explicit DC converter references unknown AC bus {}",
                conv.ac_bus
            ))
        })?;
        let dc_bus_idx = *dc_bus_map.get(&conv.dc_bus).ok_or_else(|| {
            AcOpfError::InvalidNetwork(format!(
                "explicit DC converter references unknown DC bus {}",
                conv.dc_bus
            ))
        })?;

        // Per-unit conversion matching PowerModelsACDC.jl:
        //   LossA (MW)  → / baseMVA  (power base)
        //   LossB (kV)  → / kVbase   (voltage base = basekVac)
        //   LossC (Ω)   → / Zbase    (impedance base = kVbase² / baseMVA)
        let kv_base = conv.base_kv_ac;
        let z_base = if kv_base > 1e-6 {
            kv_base * kv_base / base_mva
        } else {
            1.0
        };

        let loss_c_ohm = if conv.active_power_ac_max_mw <= 0.0 && conv.active_power_ac_min_mw < 0.0
        {
            conv.loss_quadratic_inverter
        } else {
            conv.loss_quadratic_rectifier
        };

        converters.push(HvdcConverterNlp {
            ac_bus_idx,
            dc_bus_idx,
            p_min_pu: conv.active_power_ac_min_mw / base_mva,
            p_max_pu: conv.active_power_ac_max_mw / base_mva,
            q_min_pu: conv.reactive_power_ac_min_mvar / base_mva,
            q_max_pu: conv.reactive_power_ac_max_mvar / base_mva,
            loss_a_pu: conv.loss_constant_mw / base_mva,
            loss_linear: conv.loss_linear / kv_base.max(1e-6),
            loss_c: loss_c_ohm / z_base,
            i_max_pu: conv.current_max_pu,
            dc_control,
            ac_control,
            p_dc_set_pu: conv.power_dc_setpoint_mw / base_mva,
            voltage_dc_setpoint_pu: conv.voltage_dc_setpoint_pu,
            q_ac_set_pu: conv.reactive_power_mvar / base_mva,
            voltage_ac_setpoint_pu: conv.voltage_setpoint_pu,
        });
    }

    let n_conv = converters.len();
    if n_conv == 0 {
        return Ok(None);
    }

    // Build DC bus conductance matrix G_dc from DC branches.
    let mut g_dc = vec![vec![0.0_f64; n_dc_bus]; n_dc_bus];
    for br in &dc_branches {
        if !br.status {
            continue;
        }
        let fi = *dc_bus_map.get(&br.from_bus).ok_or_else(|| {
            AcOpfError::InvalidNetwork(format!(
                "explicit DC branch references unknown from_bus {}",
                br.from_bus
            ))
        })?;
        let ti = *dc_bus_map.get(&br.to_bus).ok_or_else(|| {
            AcOpfError::InvalidNetwork(format!(
                "explicit DC branch references unknown to_bus {}",
                br.to_bus
            ))
        })?;

        let base_kv_from = dc_buses[fi].base_kv_dc;
        let base_kv_to = dc_buses[ti].base_kv_dc;
        let base_kv_dc = if (base_kv_from - base_kv_to).abs() < 1e-6 {
            base_kv_from
        } else {
            (base_kv_from + base_kv_to) / 2.0
        };
        let z_base_dc = base_kv_dc * base_kv_dc / base_mva;
        let r_pu = if z_base_dc > 1e-10 {
            br.r_ohm / z_base_dc
        } else {
            br.r_ohm
        };
        let g_branch = if r_pu.abs() > 1e-20 { 1.0 / r_pu } else { 1e6 };

        g_dc[fi][fi] += g_branch;
        g_dc[ti][ti] += g_branch;
        g_dc[fi][ti] -= g_branch;
        g_dc[ti][fi] -= g_branch;
    }

    let vdc_bounds: Vec<(f64, f64)> = network
        .hvdc
        .dc_buses()
        .map(|dcb| (dcb.v_dc_min, dcb.v_dc_max))
        .collect();

    let mut dc_bus_conv_map: Vec<Vec<usize>> = vec![vec![]; n_dc_bus];
    for (k, conv) in converters.iter().enumerate() {
        dc_bus_conv_map[conv.dc_bus_idx].push(k);
    }

    info!(
        n_conv,
        n_dc_bus, "HVDC NLP data: {} converters, {} DC buses", n_conv, n_dc_bus
    );

    Ok(Some(HvdcNlpData {
        n_conv,
        n_dc_bus,
        converters,
        g_dc,
        vdc_bounds,
        dc_bus_conv_map,
    }))
}

// ---------------------------------------------------------------------------
// Sequential fallback for point-to-point HVDC links.
// ---------------------------------------------------------------------------

/// Solve AC-OPF with HVDC links via sequential AC-DC iteration.
///
/// Used for point-to-point `network.hvdc` links rather than explicit DC-network topology.
pub fn solve_ac_opf_with_hvdc(
    network: &Network,
    options: &AcOpfOptions,
) -> Result<AcOpfHvdcResult, AcOpfError> {
    solve_ac_opf_with_hvdc_with_runtime(network, options, &AcOpfRuntime::default())
}

/// Solve AC-OPF with HVDC links using an explicit runtime context.
pub fn solve_ac_opf_with_hvdc_with_runtime(
    network: &Network,
    options: &AcOpfOptions,
    runtime: &AcOpfRuntime,
) -> Result<AcOpfHvdcResult, AcOpfError> {
    solve_ac_opf_with_hvdc_context(network, options, &AcOpfRunContext::from_runtime(runtime))
}

pub(crate) fn solve_ac_opf_with_hvdc_context(
    network: &Network,
    options: &AcOpfOptions,
    context: &AcOpfRunContext,
) -> Result<AcOpfHvdcResult, AcOpfError> {
    let links = links_from_network(network);
    if links.is_empty() {
        if network.hvdc.has_explicit_dc_topology() {
            return Err(AcOpfError::SolverError(
                "solve_ac_opf_with_hvdc only supports point-to-point HVDC links; use solve_ac_opf() for explicit DC-network topology"
                    .to_string(),
            ));
        }
        let mut inner_opts = options.clone();
        inner_opts.include_hvdc = Some(false);
        let opf = super::solve::solve_ac_opf_with_context(network, &inner_opts, context)?;
        return Ok(AcOpfHvdcResult {
            opf,
            hvdc_p_dc_mw: Vec::new(),
            hvdc_p_loss_mw: Vec::new(),
            hvdc_iterations: 0,
        });
    }

    let base_mva = network.base_mva;

    info!(
        hvdc_links = links.len(),
        max_iterations = MAX_HVDC_ITERATIONS,
        "starting AC-OPF with HVDC (sequential)"
    );

    let mut converter_results: Vec<LinkState> = links
        .iter()
        .map(|link| initial_link_state(link, base_mva))
        .collect();

    let mut last_opf: Option<OpfSolution> = None;
    let mut iterations = 0u32;
    let mut converged = false;

    // Pre-compute flat-start MTDC injections (first outer iteration).
    // These will be updated from OPF AC voltages in subsequent iterations.
    let mut mtdc_results = {
        let max_bus = network
            .buses
            .iter()
            .map(|b| b.number as usize)
            .max()
            .unwrap_or(0);
        let flat_v = vec![Complex64::new(1.0, 0.0); max_bus + 1];
        dc_grid_injections_from_voltages(network, &flat_v)
            .map_err(|error| AcOpfError::SolverError(error.to_string()))?
    };

    for _outer in 0..MAX_HVDC_ITERATIONS {
        iterations += 1;
        let mut aug_net = build_augmented_network(network, &converter_results);

        // Apply MTDC injections via Load objects on the augmented network.
        // aug_net is ephemeral and there's no contingency handling in the OPF loop.
        apply_dc_grid_injections(&mut aug_net, &mtdc_results.injections, false);

        let mut inner_opts = options.clone();
        inner_opts.include_hvdc = Some(false);
        let opf_result = super::solve::solve_ac_opf_with_context(&aug_net, &inner_opts, context)?;

        let vm_map: HashMap<u32, f64> = aug_net
            .buses
            .iter()
            .zip(opf_result.power_flow.voltage_magnitude_pu.iter())
            .map(|(b, &v)| (b.number, v))
            .collect();

        // Update MTDC injections from OPF AC voltages.
        if network.hvdc.has_explicit_dc_topology() {
            let max_bus = network
                .buses
                .iter()
                .map(|b| b.number as usize)
                .max()
                .unwrap_or(0);
            let mut ac_v = vec![Complex64::new(1.0, 0.0); max_bus + 1];
            for (bus, &mag, &ang) in aug_net
                .buses
                .iter()
                .zip(opf_result.power_flow.voltage_magnitude_pu.iter())
                .zip(opf_result.power_flow.voltage_angle_rad.iter())
                .map(|((b, vm), va)| (b, vm, va))
            {
                let idx = bus.number as usize;
                if idx < ac_v.len() {
                    ac_v[idx] = Complex64::from_polar(mag, ang);
                }
            }
            mtdc_results = dc_grid_injections_from_voltages(network, &ac_v)
                .map_err(|error| AcOpfError::SolverError(error.to_string()))?;
        }

        let new_results: Vec<LinkState> = links
            .iter()
            .map(|link: &HvdcLink| {
                let v_from = vm_map.get(&link.from_bus()).copied().unwrap_or(1.0);
                let v_to = vm_map.get(&link.to_bus()).copied().unwrap_or(1.0);
                updated_link_state(link, v_from, v_to, base_mva)
            })
            .collect();

        let max_delta = converter_results
            .iter()
            .zip(new_results.iter())
            .map(|(old, new)| {
                let dp = (old.p_from_mw - new.p_from_mw)
                    .abs()
                    .max((old.p_to_mw - new.p_to_mw).abs());
                let dq = (old.q_from_mvar - new.q_from_mvar)
                    .abs()
                    .max((old.q_to_mvar - new.q_to_mvar).abs());
                dp.max(dq)
            })
            .fold(0.0_f64, f64::max);

        debug!(
            iteration = iterations,
            max_delta_mw = max_delta,
            "HVDC AC-OPF outer iteration"
        );

        converter_results = new_results;
        last_opf = Some(opf_result);

        if max_delta < HVDC_CONVERGENCE_TOL_MW {
            converged = true;
            break;
        }
    }

    if !converged {
        warn!(
            iterations,
            "AC-OPF HVDC sequential iteration did not converge"
        );
        return Err(AcOpfError::NotConverged);
    } else {
        info!(
            iterations,
            n_links = links.len(),
            "AC-OPF HVDC sequential iteration converged"
        );
    }

    let opf = last_opf.expect("at least one OPF iteration must have run");
    let hvdc_p_dc_mw: Vec<f64> = converter_results.iter().map(|r| r.p_dc_mw).collect();
    let hvdc_p_loss_mw: Vec<f64> = converter_results.iter().map(|r| r.p_loss_mw).collect();

    Ok(AcOpfHvdcResult {
        opf,
        hvdc_p_dc_mw,
        hvdc_p_loss_mw,
        hvdc_iterations: iterations,
    })
}

// ---------------------------------------------------------------------------
// Sequential helpers
// ---------------------------------------------------------------------------

/// Per-link converter state for AC-OPF HVDC sequential iteration.
///
/// Tracks from/to bus P/Q injections for convergence checking. This is
/// internal to the AC-OPF HVDC code; the public API uses `StationSolution`.
#[derive(Clone)]
struct LinkState {
    from_bus: u32,
    to_bus: u32,
    p_from_mw: f64,
    q_from_mvar: f64,
    p_to_mw: f64,
    q_to_mvar: f64,
    p_dc_mw: f64,
    p_loss_mw: f64,
}

fn initial_link_state(link: &HvdcLink, base_mva: f64) -> LinkState {
    match link {
        HvdcLink::Lcc(p) => {
            let p_dc = p.p_dc_mw;
            let p_loss = p.r_dc_pu * (p_dc / base_mva).powi(2) * base_mva;
            let q_rect = p.q_rectifier_mvar(p_dc);
            let q_inv = p.q_inverter_mvar(p_dc);
            LinkState {
                from_bus: p.from_bus,
                to_bus: p.to_bus,
                p_from_mw: -(p_dc + p_loss),
                q_from_mvar: -q_rect,
                p_to_mw: p_dc,
                q_to_mvar: -q_inv,
                p_dc_mw: p_dc,
                p_loss_mw: p_loss,
            }
        }
        HvdcLink::Vsc(p) => {
            let p_dc = p.p_dc_mw;
            let i_ac_pu = p_dc.abs() / base_mva;
            let p_loss = p.losses_mw(i_ac_pu, base_mva);
            LinkState {
                from_bus: p.from_bus,
                to_bus: p.to_bus,
                p_from_mw: -(p_dc + p_loss),
                q_from_mvar: -p.q_from_mvar,
                p_to_mw: p_dc,
                q_to_mvar: -p.q_to_mvar,
                p_dc_mw: p_dc,
                p_loss_mw: p_loss,
            }
        }
    }
}

fn updated_link_state(link: &HvdcLink, v_from: f64, _v_to: f64, base_mva: f64) -> LinkState {
    match link {
        HvdcLink::Lcc(p) => {
            let p_dc = p.p_dc_mw;
            let k_r = (3.0 * 2.0_f64.sqrt() / std::f64::consts::PI)
                * p.a_r
                * p.firing_angle_deg.to_radians().cos();
            let v_d_r = k_r * v_from * base_mva.sqrt();
            let i_dc = if v_d_r.abs() > 1e-6 {
                p_dc / v_d_r
            } else {
                0.0
            };
            let p_loss = p.r_dc_pu * i_dc * i_dc * base_mva;
            let q_rect = p.q_rectifier_mvar(p_dc);
            let q_inv = p.q_inverter_mvar(p_dc);
            LinkState {
                from_bus: p.from_bus,
                to_bus: p.to_bus,
                p_from_mw: -(p_dc + p_loss),
                q_from_mvar: -q_rect,
                p_to_mw: p_dc,
                q_to_mvar: -q_inv,
                p_dc_mw: p_dc,
                p_loss_mw: p_loss,
            }
        }
        HvdcLink::Vsc(p) => {
            let p_dc = p.p_dc_mw;
            let s_ac = ((p_dc * p_dc) + (p.q_from_mvar * p.q_from_mvar)).sqrt();
            let i_ac_pu = if v_from > 1e-6 {
                s_ac / (v_from * base_mva)
            } else {
                0.0
            };
            let p_loss = p.losses_mw(i_ac_pu, base_mva);
            LinkState {
                from_bus: p.from_bus,
                to_bus: p.to_bus,
                p_from_mw: -(p_dc + p_loss),
                q_from_mvar: -p.q_from_mvar,
                p_to_mw: p_dc,
                q_to_mvar: -p.q_to_mvar,
                p_dc_mw: p_dc,
                p_loss_mw: p_loss,
            }
        }
    }
}

fn build_augmented_network(network: &Network, converters: &[LinkState]) -> Network {
    let mut aug = network.clone();
    aug.hvdc.links.clear();
    aug.hvdc.clear_dc_grids();

    let mut p_delta: HashMap<u32, f64> = HashMap::new();
    let mut q_delta: HashMap<u32, f64> = HashMap::new();

    for res in converters {
        *p_delta.entry(res.from_bus).or_default() += res.p_from_mw;
        *q_delta.entry(res.from_bus).or_default() += res.q_from_mvar;
        *p_delta.entry(res.to_bus).or_default() += res.p_to_mw;
        *q_delta.entry(res.to_bus).or_default() += res.q_to_mvar;
    }

    for (bus_num, &p_mw) in &p_delta {
        let q_mvar = q_delta.get(bus_num).copied().unwrap_or(0.0);
        aug.loads.push(Load::new(*bus_num, -p_mw, -q_mvar));
    }

    let hvdc_buses: std::collections::HashSet<u32> = p_delta.keys().copied().collect();
    for bus in aug.buses.iter_mut() {
        if hvdc_buses.contains(&bus.number) && bus.bus_type == BusType::Isolated {
            bus.bus_type = BusType::PQ;
        }
    }

    aug
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::market::CostCurve;
    use surge_network::network::Branch;
    use surge_network::network::{Bus, BusType, DcBus, DcConverterStation, Generator, Load};

    fn three_bus_network() -> Network {
        let mut net = Network::new("hvdc-acopf-test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_angle_rad = 0.0;
        net.buses.push(b1);

        let mut b2 = Bus::new(2, BusType::PV, 230.0);
        b2.voltage_magnitude_pu = 1.0;
        net.buses.push(b2);

        let b3 = Bus::new(3, BusType::PQ, 230.0);
        net.buses.push(b3);
        net.loads.push(Load::new(3, 50.0, 10.0));

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(1, 3, 0.02, 0.06, 0.03));
        net.branches.push(Branch::new_line(2, 3, 0.015, 0.04, 0.02));

        let mut g1 = Generator::new(1, 80.0, 1.0);
        g1.pmax = 200.0;
        g1.pmin = 0.0;
        g1.qmax = 100.0;
        g1.qmin = -100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 20.0, 0.0],
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(2, 30.0, 1.0);
        g2.pmax = 100.0;
        g2.pmin = 0.0;
        g2.qmax = 80.0;
        g2.qmin = -80.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.03, 25.0, 0.0],
        });
        net.generators.push(g2);

        net.loads.push(Load::new(3, 50.0, 10.0));

        net
    }

    fn three_bus_network_with_explicit_hvdc() -> Network {
        use surge_network::network::DcBranch;

        let mut net = three_bus_network();
        let grid = net.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.buses.push(DcBus {
            bus_id: 102,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });

        let conv_template = DcConverterStation {
            id: String::new(),
            dc_bus: 101,
            ac_bus: 1,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 0.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 10.0,
            status: true,
            loss_constant_mw: 1.0,
            loss_linear: 0.188,
            loss_quadratic_rectifier: 0.01,
            loss_quadratic_inverter: 0.01,
            droop: 0.0,
            power_dc_setpoint_mw: 20.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        };
        let grid = net.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters.push(conv_template.clone().into());
        let mut conv2 = conv_template;
        conv2.dc_bus = 102;
        conv2.ac_bus = 2;
        conv2.control_type_dc = 2;
        conv2.loss_constant_mw = 0.5;
        conv2.loss_linear = 0.188;
        conv2.loss_quadratic_rectifier = 0.01;
        conv2.loss_quadratic_inverter = 0.01;
        conv2.power_dc_setpoint_mw = 0.0;
        grid.converters.push(conv2.into());

        grid.branches.push(DcBranch {
            id: String::new(),
            from_bus: 101,
            to_bus: 102,
            r_ohm: 5.0,
            l_mh: 0.0,
            c_uf: 0.0,
            rating_a_mva: 100.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });

        net
    }

    #[test]
    fn test_ac_opf_no_hvdc_unchanged() {
        let nlp = match crate::backends::try_default_nlp_solver() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: no NLP solver available");
                return;
            }
        };

        let net = three_bus_network();

        let opts_auto = AcOpfOptions {
            include_hvdc: None,
            ..AcOpfOptions::default()
        };
        let runtime_auto = AcOpfRuntime::default().with_nlp_solver(nlp.clone());
        let sol_auto =
            crate::ac::solve_ac_opf_with_runtime(&net, &opts_auto, &runtime_auto).unwrap();

        let opts_off = AcOpfOptions {
            include_hvdc: Some(false),
            ..AcOpfOptions::default()
        };
        let runtime_off = AcOpfRuntime::default().with_nlp_solver(nlp.clone());
        let sol_off = crate::ac::solve_ac_opf_with_runtime(&net, &opts_off, &runtime_off).unwrap();

        assert!(
            (sol_auto.total_cost - sol_off.total_cost).abs() < 1e-3,
            "total cost mismatch: auto={} off={}",
            sol_auto.total_cost,
            sol_off.total_cost
        );
    }

    #[test]
    fn test_build_hvdc_nlp_data_none_when_empty() {
        let net = three_bus_network();
        assert!(build_hvdc_nlp_data(&net).unwrap().is_none());
    }

    #[test]
    fn test_build_hvdc_nlp_data_constructs_g_dc() {
        use surge_network::network::{DcBranch, DcBus, DcConverterStation};

        let mut net = three_bus_network();
        let grid = net.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.buses.push(DcBus {
            bus_id: 102,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });

        let conv_template = DcConverterStation {
            id: String::new(),
            dc_bus: 101,
            ac_bus: 1,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 0.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 10.0,
            status: true,
            loss_constant_mw: 1.1,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 20.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        };
        let grid = net.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters.push(conv_template.clone().into());
        let mut conv2 = conv_template;
        conv2.dc_bus = 102;
        conv2.ac_bus = 2;
        conv2.control_type_dc = 2;
        conv2.loss_constant_mw = 0.9;
        conv2.power_dc_setpoint_mw = 0.0;
        grid.converters.push(conv2.into());

        grid.branches.push(DcBranch {
            id: String::new(),
            from_bus: 101,
            to_bus: 102,
            r_ohm: 5.0,
            l_mh: 0.0,
            c_uf: 0.0,
            rating_a_mva: 100.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });

        let data = build_hvdc_nlp_data(&net)
            .expect("should build HVDC NLP data")
            .expect("explicit DC topology should produce HVDC NLP data");
        assert_eq!(data.n_conv, 2);
        assert_eq!(data.n_dc_bus, 2);
        assert!(data.g_dc[0][0] > 0.0);
        assert!(data.g_dc[0][1] < 0.0);
        assert!((data.g_dc[0][1] - data.g_dc[1][0]).abs() < 1e-10);
        assert!((data.g_dc[0][0] + data.g_dc[0][1]).abs() < 1e-10);
    }

    #[test]
    fn test_build_hvdc_nlp_data_maps_converter_control_contract() {
        let net = three_bus_network_with_explicit_hvdc();
        let data = build_hvdc_nlp_data(&net)
            .expect("should build HVDC NLP data")
            .expect("explicit DC topology should produce HVDC NLP data");

        assert_eq!(data.converters.len(), 2);
        assert_eq!(data.converters[0].dc_control, HvdcDcControlMode::Power);
        assert_eq!(
            data.converters[0].ac_control,
            HvdcAcControlMode::ReactivePower
        );
        assert!((data.converters[0].p_dc_set_pu - 0.2).abs() < 1e-12);
        assert_eq!(data.converters[1].dc_control, HvdcDcControlMode::Voltage);
        assert!((data.converters[1].voltage_dc_setpoint_pu - 1.0).abs() < 1e-12);
    }

    #[test]
    fn test_ac_opf_hvdc_joint_nlp() {
        let nlp = match crate::backends::nlp_solver_from_str("ipopt") {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: Ipopt not available");
                return;
            }
        };

        let net = three_bus_network_with_explicit_hvdc();

        let opts = AcOpfOptions {
            include_hvdc: None,
            enforce_thermal_limits: false,
            ..AcOpfOptions::default()
        };
        let runtime = AcOpfRuntime::default().with_nlp_solver(nlp);

        let sol = crate::ac::solve_ac_opf_with_runtime(&net, &opts, &runtime);
        assert!(
            sol.is_ok(),
            "AC-OPF with HVDC joint NLP failed: {:?}",
            sol.err()
        );
        let sol = sol.unwrap();
        assert!(sol.total_cost > 0.0, "objective should be positive");
    }

    #[test]
    fn test_ac_opf_hvdc_screening_surrogate_matches_unscreened() {
        let nlp = match crate::backends::nlp_solver_from_str("ipopt") {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: Ipopt not available");
                return;
            }
        };

        let mut net = three_bus_network_with_explicit_hvdc();
        for branch in &mut net.branches {
            branch.rating_a_mva = 200.0;
        }

        let opts_full = AcOpfOptions {
            include_hvdc: None,
            enforce_thermal_limits: true,
            constraint_screening_threshold: None,
            ..AcOpfOptions::default()
        };
        let runtime_full = AcOpfRuntime::default().with_nlp_solver(nlp.clone());
        let full = crate::ac::solve_ac_opf_with_runtime(&net, &opts_full, &runtime_full)
            .expect("unscreened explicit-HVDC AC-OPF should converge");

        let opts_screened = AcOpfOptions {
            include_hvdc: None,
            enforce_thermal_limits: true,
            constraint_screening_threshold: Some(1.5),
            constraint_screening_min_buses: 0,
            screening_fallback_enabled: true,
            ..AcOpfOptions::default()
        };
        let runtime_screened = AcOpfRuntime::default().with_nlp_solver(nlp);
        let screened =
            crate::ac::solve_ac_opf_with_runtime(&net, &opts_screened, &runtime_screened)
                .expect("screened explicit-HVDC AC-OPF should converge");

        assert!(
            (full.total_cost - screened.total_cost).abs() < 1e-3,
            "screened explicit-HVDC AC-OPF should match unscreened cost: full={} screened={}",
            full.total_cost,
            screened.total_cost
        );
    }

    #[test]
    fn test_build_hvdc_nlp_data_rejects_bidirectional_asymmetric_losses() {
        let mut net = three_bus_network();
        let grid = net.hvdc.ensure_dc_grid(1, Some("grid".to_string()));

        grid.buses.push(DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });

        grid.converters.push(
            DcConverterStation {
                id: String::new(),
                dc_bus: 101,
                ac_bus: 1,
                control_type_dc: 1,
                control_type_ac: 1,
                active_power_mw: 0.0,
                reactive_power_mvar: 0.0,
                is_lcc: false,
                voltage_setpoint_pu: 1.0,
                transformer_r_pu: 0.0,
                transformer_x_pu: 0.0,
                transformer: false,
                tap_ratio: 1.0,
                filter_susceptance_pu: 0.0,
                filter: false,
                reactor_r_pu: 0.0,
                reactor_x_pu: 0.0,
                reactor: false,
                base_kv_ac: 230.0,
                voltage_max_pu: 1.1,
                voltage_min_pu: 0.9,
                current_max_pu: 10.0,
                status: true,
                loss_constant_mw: 0.5,
                loss_linear: 0.0,
                loss_quadratic_rectifier: 0.01,
                loss_quadratic_inverter: 0.02,
                droop: 0.0,
                power_dc_setpoint_mw: 10.0,
                voltage_dc_setpoint_pu: 1.0,
                active_power_ac_max_mw: 100.0,
                active_power_ac_min_mw: -100.0,
                reactive_power_ac_max_mvar: 50.0,
                reactive_power_ac_min_mvar: -50.0,
            }
            .into(),
        );

        let err = match build_hvdc_nlp_data(&net) {
            Ok(_) => panic!("asymmetric bidirectional losses must be rejected"),
            Err(err) => err,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("bidirectional converters with asymmetric quadratic losses"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_ac_opf_hvdc_auto_detect() {
        let nlp = match crate::backends::try_default_nlp_solver() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: no NLP solver available");
                return;
            }
        };

        let mut net = three_bus_network();
        use surge_network::network::{
            VscConverterAcControlMode, VscConverterTerminal, VscHvdcControlMode, VscHvdcLink,
        };
        net.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-test".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            converter1: VscConverterTerminal {
                bus: 1,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 20.0,
                ac_setpoint: 0.0,
                loss_constant_mw: 0.5,
                loss_linear: 0.01,
                q_min_mvar: -50.0,
                q_max_mvar: 50.0,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
            converter2: VscConverterTerminal {
                bus: 2,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 0.0,
                ac_setpoint: 0.0,
                loss_constant_mw: 0.3,
                loss_linear: 0.01,
                q_min_mvar: -50.0,
                q_max_mvar: 50.0,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
        });

        let opts = AcOpfOptions {
            include_hvdc: None,
            ..AcOpfOptions::default()
        };
        let runtime = AcOpfRuntime::default().with_nlp_solver(nlp);

        let sol = crate::ac::solve_ac_opf_with_runtime(&net, &opts, &runtime);
        assert!(sol.is_ok(), "AC-OPF with HVDC auto-detect failed: {sol:?}");
        assert!(sol.unwrap().total_cost > 0.0);
    }

    #[test]
    fn test_ac_opf_hvdc_sequential_converges() {
        let nlp = match crate::backends::try_default_nlp_solver() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: no NLP solver available");
                return;
            }
        };

        let mut net = three_bus_network();
        use surge_network::network::{VscConverterTerminal, VscHvdcControlMode, VscHvdcLink};
        net.hvdc.push_vsc_link(VscHvdcLink {
            name: "VSC-seq".to_string(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            converter1: VscConverterTerminal {
                bus: 1,
                dc_setpoint: 15.0,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 3,
                ..VscConverterTerminal::default()
            },
        });

        let opts = AcOpfOptions {
            ..AcOpfOptions::default()
        };
        let runtime = AcOpfRuntime::default().with_nlp_solver(nlp);

        let result =
            solve_ac_opf_with_hvdc_context(&net, &opts, &AcOpfRunContext::from_runtime(&runtime));
        assert!(result.is_ok(), "HVDC sequential solve failed: {result:?}");
        let r = result.unwrap();
        assert!(r.hvdc_iterations <= MAX_HVDC_ITERATIONS);
        assert_eq!(r.hvdc_p_dc_mw.len(), 1);
        assert!(r.opf.total_cost > 0.0);
    }

    /// Multi-island HVDC: two AC components connected only by a DC link.
    /// The AC B' matrix is singular (disconnected). This verifies the PTDF
    /// LMP decomposition gracefully falls back instead of crashing.
    #[test]
    fn test_ac_opf_hvdc_multi_island_ptdf_fallback() {
        let nlp = match crate::backends::try_default_nlp_solver() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: no NLP solver available");
                return;
            }
        };

        use surge_network::network::{DcBranch, DcBus, DcConverterStation};

        // Build a 4-bus, 2-island network:
        // Island 1: bus 1 (slack+gen) -- bus 2 (load)
        // Island 2: bus 3 (gen) -- bus 4 (load)
        // Connected only by HVDC: converter at bus 2 <-> converter at bus 3
        let mut net = Network::new("multi-island-hvdc");
        net.base_mva = 100.0;

        // Island 1
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        net.buses.push(b1);
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 30.0, 5.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));

        // Island 2
        let mut b3 = Bus::new(3, BusType::Slack, 230.0);
        b3.voltage_magnitude_pu = 1.0;
        net.buses.push(b3);
        let b4 = Bus::new(4, BusType::PQ, 230.0);
        net.buses.push(b4);
        net.loads.push(Load::new(4, 40.0, 8.0));
        net.branches.push(Branch::new_line(3, 4, 0.015, 0.04, 0.02));

        // Generators
        let mut g1 = Generator::new(1, 60.0, 1.0);
        g1.pmax = 200.0;
        g1.pmin = 0.0;
        g1.qmax = 100.0;
        g1.qmin = -100.0;
        g1.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.02, 20.0, 0.0],
        });
        net.generators.push(g1);

        let mut g2 = Generator::new(3, 50.0, 1.0);
        g2.pmax = 100.0;
        g2.pmin = 0.0;
        g2.qmax = 80.0;
        g2.qmin = -80.0;
        g2.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.03, 25.0, 0.0],
        });
        net.generators.push(g2);

        // HVDC link connecting island 1 (bus 2) to island 2 (bus 3)
        let grid = net.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
            bus_id: 201,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.buses.push(DcBus {
            bus_id: 202,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 345.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });

        let conv_template = DcConverterStation {
            id: String::new(),
            dc_bus: 201,
            ac_bus: 2,
            control_type_dc: 1,
            control_type_ac: 1,
            active_power_mw: 0.0,
            reactive_power_mvar: 0.0,
            is_lcc: false,
            voltage_setpoint_pu: 1.0,
            transformer_r_pu: 0.0,
            transformer_x_pu: 0.0,
            transformer: false,
            tap_ratio: 1.0,
            filter_susceptance_pu: 0.0,
            filter: false,
            reactor_r_pu: 0.0,
            reactor_x_pu: 0.0,
            reactor: false,
            base_kv_ac: 230.0,
            voltage_max_pu: 1.1,
            voltage_min_pu: 0.9,
            current_max_pu: 10.0,
            status: true,
            loss_constant_mw: 0.5,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 10.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 100.0,
            active_power_ac_min_mw: -100.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        };
        let grid = net.hvdc.find_dc_grid_mut(1).expect("grid exists");
        grid.converters.push(conv_template.clone().into());

        let mut conv2 = conv_template;
        conv2.dc_bus = 202;
        conv2.ac_bus = 3;
        conv2.control_type_dc = 2;
        conv2.loss_constant_mw = 0.5;
        grid.converters.push(conv2.into());

        grid.branches.push(DcBranch {
            id: String::new(),
            from_bus: 201,
            to_bus: 202,
            r_ohm: 5.0,
            l_mh: 0.0,
            c_uf: 0.0,
            rating_a_mva: 100.0,
            rating_b_mva: 0.0,
            rating_c_mva: 0.0,
            status: true,
        });

        // Solve — should NOT crash on singular B' for PTDF LMP decomposition
        let opts = AcOpfOptions {
            include_hvdc: None,
            enforce_thermal_limits: true,
            ..AcOpfOptions::default()
        };
        let runtime = AcOpfRuntime::default().with_nlp_solver(nlp);

        let sol = crate::ac::solve_ac_opf_with_runtime(&net, &opts, &runtime);
        assert!(
            sol.is_ok(),
            "Multi-island HVDC AC-OPF should not crash: {:?}",
            sol.err()
        );
        let sol = sol.unwrap();
        assert!(sol.total_cost > 0.0, "objective should be positive");
        // LMP should still be populated (from direct Ipopt multipliers)
        assert_eq!(sol.pricing.lmp.len(), 4);
    }

    #[test]
    fn test_ac_opf_hvdc_empty_links_passthrough() {
        let nlp = match crate::backends::try_default_nlp_solver() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIP: no NLP solver available");
                return;
            }
        };

        let net = three_bus_network();
        let opts = AcOpfOptions {
            include_hvdc: Some(true),
            ..AcOpfOptions::default()
        };
        let runtime = AcOpfRuntime::default().with_nlp_solver(nlp);

        let result =
            solve_ac_opf_with_hvdc_context(&net, &opts, &AcOpfRunContext::from_runtime(&runtime));
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.hvdc_p_dc_mw.len(), 0);
        assert_eq!(r.hvdc_iterations, 0);
    }
}
