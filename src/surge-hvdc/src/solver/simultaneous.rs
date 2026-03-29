// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Experimental simultaneous AC/DC Newton solver.
//!
//! This module implements a true coupled algebraic solve for MTDC studies:
//! one state vector, one residual, one Newton step per iteration.
//! Compared with the block-coupled outer iteration, this path keeps the AC and
//! DC equations in one Newton loop and explicitly accounts for the AC-side
//! active-power injection of the DC slack station.
//!
//! Supports:
//! - VSC stations: `ConstantPQ`, `ConstantPVac`, `ConstantVdc`, `PVdcDroop`
//! - LCC stations: constant-power current source with analytically derived
//!   firing angle and reactive power absorption
//! - All-droop configurations (no `ConstantVdc` required when `PVdcDroop`
//!   stations are present)
//! - Coordinated multi-station droop (post-iteration power redistribution)
//! - dense analytic Jacobian (appropriate for modest MTDC state sizes)

use std::collections::HashMap;

use faer::sparse::SparseColMat;
use surge_ac::matrix::jacobian::build_jacobian;
use surge_ac::matrix::mismatch::compute_power_injection;
use surge_ac::matrix::ybus::{YBus, build_ybus};
use surge_network::Network;
use surge_network::network::BusType;
use tracing::{debug, info, warn};

use crate::dc_network::topology::{DcNetwork, DcPfResult, solve_system};
use crate::error::HvdcError;
use crate::model::control::VscHvdcControlMode;
use crate::solver::block_coupled::{
    AcDcMethod, BlockCoupledAcDcResult, BlockCoupledAcDcSolverOptions, VscStation,
    VscStationResult, solve_block_coupled_ac_dc,
};
use crate::solver::hybrid_mtdc::LccConverter;

// Physical constants for LCC 6-pulse bridge converter.
/// (3√2/π) — ideal no-load DC voltage ratio.
const K_BRIDGE: f64 = 3.0 * std::f64::consts::SQRT_2 / std::f64::consts::PI;

const NONE: usize = usize::MAX;

/// Options for the experimental simultaneous AC/DC Newton solver.
#[derive(Debug, Clone)]
pub struct SimultaneousAcDcSolverOptions {
    /// Convergence tolerance on the coupled residual infinity norm (pu).
    pub tol: f64,
    /// Maximum simultaneous Newton iterations.
    pub max_iter: usize,
    /// Maximum number of backtracking halvings per Newton step.
    pub max_line_search_steps: usize,
    /// Enable coordinated multi-station droop (post-iteration power
    /// redistribution across PVdcDroop stations proportional to their gains).
    pub coordinated_droop: bool,
}

impl Default for SimultaneousAcDcSolverOptions {
    fn default() -> Self {
        Self {
            tol: 1e-6,
            max_iter: 20,
            max_line_search_steps: 8,
            coordinated_droop: true,
        }
    }
}

#[derive(Clone)]
struct SimultaneousLayout {
    pvpq: Vec<usize>,
    pq: Vec<usize>,
    dc_free: Vec<usize>,
    pvac_stations: Vec<usize>,
    theta_pos: Vec<usize>,
    vm_pos: Vec<usize>,
    dc_pos: Vec<usize>,
    q_pos: Vec<usize>,
}

impl SimultaneousLayout {
    fn new(network: &Network, dc_network: &DcNetwork, stations: &[VscStation]) -> Self {
        let n_bus = network.buses.len();
        let mut pvpq = Vec::new();
        let mut pq = Vec::new();
        for (idx, bus) in network.buses.iter().enumerate() {
            match bus.bus_type {
                BusType::PV => pvpq.push(idx),
                BusType::PQ => {
                    pvpq.push(idx);
                    pq.push(idx);
                }
                BusType::Slack | BusType::Isolated => {}
            }
        }
        pvpq.sort_unstable();

        let dc_free: Vec<usize> = (0..dc_network.n_buses())
            .filter(|&k| k != dc_network.slack_dc_bus)
            .collect();
        let pvac_stations: Vec<usize> = stations
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.control_mode.is_voltage_regulating().then_some(i))
            .collect();

        let mut theta_pos = vec![NONE; n_bus];
        for (pos, &bus) in pvpq.iter().enumerate() {
            theta_pos[bus] = pos;
        }
        let mut vm_pos = vec![NONE; n_bus];
        let vm_offset = pvpq.len();
        for (pos, &bus) in pq.iter().enumerate() {
            vm_pos[bus] = vm_offset + pos;
        }
        let mut dc_pos = vec![NONE; dc_network.n_buses()];
        let dc_offset = pvpq.len() + pq.len();
        for (pos, &bus) in dc_free.iter().enumerate() {
            dc_pos[bus] = dc_offset + pos;
        }
        let mut q_pos = vec![NONE; stations.len()];
        let q_offset = dc_offset + dc_free.len();
        for (pos, &station_idx) in pvac_stations.iter().enumerate() {
            q_pos[station_idx] = q_offset + pos;
        }

        Self {
            pvpq,
            pq,
            dc_free,
            pvac_stations,
            theta_pos,
            vm_pos,
            dc_pos,
            q_pos,
        }
    }

    fn n_state(&self) -> usize {
        self.pvpq.len() + self.pq.len() + self.dc_free.len() + self.pvac_stations.len()
    }

    fn n_residual(&self) -> usize {
        self.pvpq.len() + self.pq.len() + self.dc_free.len() + self.pvac_stations.len()
    }

    fn p_row(&self, bus: usize) -> Option<usize> {
        let pos = self.theta_pos[bus];
        (pos != NONE).then_some(pos)
    }

    fn q_row(&self, bus: usize) -> Option<usize> {
        let pos = self.vm_pos[bus];
        (pos != NONE).then_some(self.pvpq.len() + (pos - self.pvpq.len()))
    }

    fn dc_row(&self, dc_bus: usize) -> Option<usize> {
        let pos = self.dc_pos[dc_bus];
        (pos != NONE)
            .then_some(self.pvpq.len() + self.pq.len() + (pos - self.pvpq.len() - self.pq.len()))
    }

    fn q_control_row(&self, station_idx: usize) -> Option<usize> {
        let pos = self.q_pos[station_idx];
        (pos != NONE).then_some(
            self.pvpq.len()
                + self.pq.len()
                + self.dc_free.len()
                + (pos - self.pvpq.len() - self.pq.len() - self.dc_free.len()),
        )
    }
}

#[derive(Clone)]
enum PvacConstraint {
    VoltageTarget { target_vm_pu: f64 },
    ReactiveLimit { target_q_mvar: f64 },
}

#[derive(Clone)]
struct StationOperatingPoint {
    net_p_mw: f64,
    q_mvar: f64,
    losses_mw: f64,
    dnet_dvm_mw: f64,
    dnet_dvdc_mw: f64,
    dnet_dq_mw_per_mvar: f64,
    pvac_constraint: Option<PvacConstraint>,
}

/// LCC converter operating point for one Newton evaluation.
#[derive(Clone)]
struct LccOperatingPoint {
    /// AC-side real power injection in MW (negative = rectifier draws from AC).
    _p_ac_mw: f64,
    /// AC-side reactive power in MVAR (always ≤ 0: LCC absorbs Q).
    _q_mvar: f64,
    /// ∂Q_lcc/∂V_ac in MVAR/pu.
    dq_dvm: f64,
    /// ∂Q_lcc/∂V_dc in MVAR/pu.
    dq_dvdc: f64,
}

#[derive(Clone)]
struct SimultaneousEvaluation {
    residual: Vec<f64>,
    residual_norm: f64,
    voltage_magnitude_pu: Vec<f64>,
    voltage_angle_rad: Vec<f64>,
    v_dc: Vec<f64>,
    station_points: Vec<StationOperatingPoint>,
    lcc_points: Vec<LccOperatingPoint>,
}

fn sparse_to_dense(jac: &SparseColMat<usize, f64>, dim: usize) -> Vec<Vec<f64>> {
    let mut dense = vec![vec![0.0; dim]; dim];
    let jac_ref = jac.as_ref();
    let symbolic = jac_ref.symbolic();
    let col_ptrs: Vec<usize> = symbolic.col_ptr().to_vec();
    let row_indices: Vec<usize> = symbolic.row_idx().to_vec();
    let values = jac_ref.val();

    for col in 0..dim {
        for idx in col_ptrs[col]..col_ptrs[col + 1] {
            let row = row_indices[idx];
            dense[row][col] = values[idx];
        }
    }

    dense
}

fn max_abs(values: &[f64]) -> f64 {
    values.iter().fold(0.0_f64, |acc, v| acc.max(v.abs()))
}

fn validate_station_configuration(
    network: &Network,
    station: &VscStation,
    dc_network: &DcNetwork,
    bus_map: &HashMap<u32, usize>,
) -> Result<(), HvdcError> {
    let Some(&ac_idx) = bus_map.get(&station.ac_bus) else {
        return Err(HvdcError::BusNotFound(station.ac_bus));
    };

    if station.control_mode.is_voltage_regulating() && network.buses[ac_idx].bus_type != BusType::PQ
    {
        return Err(HvdcError::UnsupportedConfiguration(format!(
            "ConstantPVac station at AC bus {} must be attached to a PQ bus in the simultaneous solver",
            station.ac_bus
        )));
    }

    if let VscHvdcControlMode::ConstantVdc { v_dc_target, .. } = station.control_mode
        && station.dc_bus_idx != dc_network.slack_dc_bus
    {
        return Err(HvdcError::UnsupportedConfiguration(format!(
            "ConstantVdc station at AC bus {} must be placed on the DC slack bus in the simultaneous solver (target {:.4} pu configured on bus {})",
            station.ac_bus, v_dc_target, station.dc_bus_idx
        )));
    }

    Ok(())
}

fn validate_solver_configuration(
    network: &Network,
    dc_network: &DcNetwork,
    stations: &[VscStation],
    lcc_stations: &[LccConverter],
    bus_map: &HashMap<u32, usize>,
) -> Result<(), HvdcError> {
    if network.slack_bus_index().is_none() {
        return Err(HvdcError::UnsupportedConfiguration(
            "simultaneous AC/DC solve requires an AC slack bus".to_string(),
        ));
    }

    let mut n_constant_vdc = 0;
    let mut n_droop = 0;
    for station in stations {
        validate_station_configuration(network, station, dc_network, bus_map)?;
        if station.control_mode.is_dc_slack() {
            n_constant_vdc += 1;
        }
        if station.control_mode.is_droop() {
            n_droop += 1;
        }
    }

    // Validate LCC stations: AC bus must exist in network.
    for lcc in lcc_stations {
        if !bus_map.contains_key(&lcc.bus_ac) {
            return Err(HvdcError::BusNotFound(lcc.bus_ac));
        }
        if lcc.bus_dc >= dc_network.n_buses() {
            return Err(HvdcError::UnsupportedConfiguration(format!(
                "LCC converter at AC bus {} references DC bus {} which exceeds DC network size {}",
                lcc.bus_ac,
                lcc.bus_dc,
                dc_network.n_buses()
            )));
        }
    }

    // 3F: Allow all-droop configurations (no ConstantVdc) when at least one
    // PVdcDroop station exists. The largest-gain droop station becomes the
    // numerical DC slack.
    if n_constant_vdc == 0 && n_droop == 0 {
        return Err(HvdcError::UnsupportedConfiguration(
            "simultaneous AC/DC solve requires either one ConstantVdc station \
             or at least one PVdcDroop station for DC voltage reference"
                .to_string(),
        ));
    }
    if n_constant_vdc > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "simultaneous AC/DC solve supports at most one ConstantVdc station".to_string(),
        ));
    }

    Ok(())
}

/// Select the droop station with the largest |k_droop| as the numerical DC
/// slack when no ConstantVdc station is present.  Returns the station index
/// and its droop parameters, or `None` if a ConstantVdc station exists.
fn select_droop_reference(stations: &[VscStation]) -> Option<(usize, f64, f64)> {
    // Only activate when no ConstantVdc station exists.
    if stations.iter().any(|s| s.control_mode.is_dc_slack()) {
        return None;
    }
    stations
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            if let VscHvdcControlMode::PVdcDroop {
                voltage_dc_setpoint_pu,
                k_droop,
                ..
            } = &s.control_mode
            {
                Some((i, *voltage_dc_setpoint_pu, k_droop.abs()))
            } else {
                None
            }
        })
        .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, v_dc_set, _)| (i, v_dc_set, 0.0))
}

fn pvac_constraint(mode: &VscHvdcControlMode, v_ac: f64, q_mvar: f64) -> Option<PvacConstraint> {
    let VscHvdcControlMode::ConstantPVac {
        v_target,
        v_band,
        q_min,
        q_max,
        ..
    } = mode
    else {
        return None;
    };

    let q_eps = 1e-6;
    if v_ac < v_target - v_band && q_mvar >= q_max - q_eps {
        Some(PvacConstraint::ReactiveLimit {
            target_q_mvar: *q_max,
        })
    } else if v_ac > v_target + v_band && q_mvar <= q_min + q_eps {
        Some(PvacConstraint::ReactiveLimit {
            target_q_mvar: *q_min,
        })
    } else {
        Some(PvacConstraint::VoltageTarget {
            target_vm_pu: *v_target,
        })
    }
}

fn unpack_state(
    network: &Network,
    dc_network: &DcNetwork,
    layout: &SimultaneousLayout,
    stations: &[VscStation],
    x: &[f64],
) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut va: Vec<f64> = network.buses.iter().map(|b| b.voltage_angle_rad).collect();
    let mut vm: Vec<f64> = network
        .buses
        .iter()
        .map(|b| b.voltage_magnitude_pu.max(0.1))
        .collect();
    let mut v_dc = dc_network.v_dc.clone();
    let mut q_state = vec![0.0; stations.len()];

    for (bus, &pos) in layout.theta_pos.iter().enumerate() {
        if pos != NONE {
            va[bus] = x[pos];
        }
    }
    for (bus, &pos) in layout.vm_pos.iter().enumerate() {
        if pos != NONE {
            vm[bus] = x[pos].clamp(0.1, 2.5);
        }
    }
    for (dc_bus, &pos) in layout.dc_pos.iter().enumerate() {
        if pos != NONE {
            v_dc[dc_bus] = x[pos].clamp(0.05, 2.5);
        }
    }
    v_dc[dc_network.slack_dc_bus] = dc_network.v_dc_slack;

    for (station_idx, station) in stations.iter().enumerate() {
        let pos = layout.q_pos[station_idx];
        if pos != NONE {
            q_state[station_idx] = x[pos].clamp(station.q_min_mvar, station.q_max_mvar);
        }
    }

    (va, vm, v_dc, q_state)
}

fn initial_state_vector(
    network: &Network,
    dc_network: &DcNetwork,
    layout: &SimultaneousLayout,
) -> Vec<f64> {
    let mut x = vec![0.0; layout.n_state()];

    for (bus, &pos) in layout.theta_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = network.buses[bus].voltage_angle_rad;
        }
    }
    for (bus, &pos) in layout.vm_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = network.buses[bus].voltage_magnitude_pu.max(0.1);
        }
    }
    for (dc_bus, &pos) in layout.dc_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = dc_network.v_dc[dc_bus].max(0.05);
        }
    }
    for &station_idx in &layout.pvac_stations {
        x[layout.q_pos[station_idx]] = 0.0;
    }

    x
}

fn warm_start_from_block_solution(
    network: &Network,
    dc_network: &mut DcNetwork,
    layout: &SimultaneousLayout,
    stations: &[VscStation],
    opts: &SimultaneousAcDcSolverOptions,
) -> Option<Vec<f64>> {
    let mut dc_guess = dc_network.clone();
    let block_opts = BlockCoupledAcDcSolverOptions {
        tol: opts.tol.min(1e-6),
        max_iter: opts.max_iter.max(10),
        apply_coupling_sensitivities: true,
        ..Default::default()
    };
    let block = solve_block_coupled_ac_dc(network, &mut dc_guess, stations, &block_opts).ok()?;
    if !block.converged {
        return None;
    }

    let mut x = initial_state_vector(network, &dc_guess, layout);
    for (bus_idx, &pos) in layout.theta_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = block
                .theta_ac
                .get(bus_idx)
                .copied()
                .unwrap_or(network.buses[bus_idx].voltage_angle_rad);
        }
    }
    for (bus_idx, &pos) in layout.vm_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = block
                .v_ac
                .get(bus_idx)
                .copied()
                .unwrap_or(network.buses[bus_idx].voltage_magnitude_pu);
        }
    }
    for (dc_bus, &pos) in layout.dc_pos.iter().enumerate() {
        if pos != NONE {
            x[pos] = block
                .dc_result
                .v_dc
                .get(dc_bus)
                .copied()
                .unwrap_or(dc_guess.v_dc[dc_bus]);
        }
    }
    for &station_idx in &layout.pvac_stations {
        let station = &stations[station_idx];
        let q_result = block
            .station_results
            .iter()
            .find(|r| r.ac_bus == station.ac_bus && r.dc_bus_idx == station.dc_bus_idx)
            .map(|r| r.q_ac_mvar);
        if let Some(q_mvar) = q_result {
            x[layout.q_pos[station_idx]] = q_mvar.clamp(station.q_min_mvar, station.q_max_mvar);
        }
    }

    dc_network.v_dc = block.dc_result.v_dc.clone();
    dc_network.v_dc[dc_network.slack_dc_bus] = dc_network.v_dc_slack;
    Some(x)
}

fn converter_loss_terms(
    station: &VscStation,
    p_cmd_mw: f64,
    q_mvar: f64,
    v_ac_pu: f64,
    base_mva: f64,
) -> (f64, f64, f64, f64) {
    let s_mva = (p_cmd_mw * p_cmd_mw + q_mvar * q_mvar).sqrt();
    if v_ac_pu <= 1e-6 || base_mva <= 1e-6 {
        let loss = station.losses_mw(p_cmd_mw, q_mvar.abs(), v_ac_pu.max(1e-6), base_mva);
        return (loss, 0.0, 0.0, 0.0);
    }

    let i_ac = s_mva / (v_ac_pu * base_mva);
    let loss_c = station.quadratic_loss_coefficient(p_cmd_mw);
    let loss =
        (station.loss_constant_mw + station.loss_linear * i_ac + loss_c * i_ac * i_ac) * base_mva;
    let loss_slope = station.loss_linear + 2.0 * loss_c * i_ac;
    let dloss_dp = if s_mva > 1e-9 {
        loss_slope * p_cmd_mw / (s_mva * v_ac_pu)
    } else {
        0.0
    };
    let dloss_dq = if s_mva > 1e-9 {
        loss_slope * q_mvar / (s_mva * v_ac_pu)
    } else {
        0.0
    };
    let dloss_dvm = -loss_slope * base_mva * i_ac / v_ac_pu;
    (loss, dloss_dp, dloss_dq, dloss_dvm)
}

fn recover_raw_power_from_net(
    station: &VscStation,
    net_p_mw: f64,
    q_mvar: f64,
    v_ac_pu: f64,
    base_mva: f64,
) -> (f64, f64) {
    let mut raw_p = net_p_mw;
    for _ in 0..8 {
        let losses = station.losses_mw(raw_p, q_mvar.abs(), v_ac_pu.max(1e-6), base_mva);
        raw_p = net_p_mw + losses;
    }
    let losses = station.losses_mw(raw_p, q_mvar.abs(), v_ac_pu.max(1e-6), base_mva);
    (raw_p, losses)
}

fn dp_cmd_dvdc(station: &VscStation, v_dc_pu: f64) -> f64 {
    match &station.control_mode {
        VscHvdcControlMode::PVdcDroop {
            p_set,
            voltage_dc_setpoint_pu,
            k_droop,
            p_min,
            p_max,
        } => {
            let p = p_set + k_droop * (v_dc_pu - voltage_dc_setpoint_pu);
            if p <= *p_min || p >= *p_max {
                0.0
            } else {
                *k_droop
            }
        }
        _ => 0.0,
    }
}

/// Compute LCC reactive power and its derivatives w.r.t. V_ac and V_dc.
///
/// LCC model: cos(φ) = V_dc / (K_BRIDGE × V_ac), Q = −|P_dc| × tan(φ).
/// Returns `(Q_mvar, dQ/dV_ac, dQ/dV_dc)`.
fn lcc_q_and_derivatives(p_dc_mw: f64, v_dc: f64, v_ac: f64) -> (f64, f64, f64) {
    let p_abs = p_dc_mw.abs();
    let denom = K_BRIDGE * v_ac;
    if denom < 1e-9 || p_abs < 1e-9 {
        return (0.0, 0.0, 0.0);
    }
    let c = (v_dc / denom).clamp(0.01, 0.999);
    let s = (1.0 - c * c).sqrt();
    let tan_phi = s / c;
    let q_mvar = -p_abs * tan_phi;

    // dQ/dc = −|P| × d(tan(φ))/dc = −|P| × (−1/(c² × √(1−c²))) = |P|/(c²×s)
    let dq_dc = p_abs / (c * c * s);

    // dc/dV_ac = −V_dc / (K_BRIDGE × V_ac²)
    let dc_dvm = -v_dc / (denom * v_ac);
    // dc/dV_dc = 1 / (K_BRIDGE × V_ac)
    let dc_dvdc = 1.0 / denom;

    let dq_dvm = dq_dc * dc_dvm; // negative: Q becomes more negative as V_ac drops
    let dq_dvdc = dq_dc * dc_dvdc; // positive: Q becomes less negative as V_dc rises

    // The sign of Q is negative (absorption), so dQ is the derivative of a
    // negative quantity.  dq_dc is positive (Q gets less negative as c rises).
    // dq_dvm: since dc_dvm < 0, dq_dvm < 0 (lower V_ac → more Q absorption).
    // dq_dvdc: since dc_dvdc > 0, dq_dvdc > 0 (higher V_dc → less Q absorption).
    // But we computed dQ/dc as positive (the magnitude of tan decreases). The
    // actual Q is −|P|×tan, so dQ/dc should incorporate the sign correctly.
    // Let's verify: Q = −|P|×s/c. If c increases slightly, s decreases, s/c
    // decreases, so Q becomes less negative → dQ/dc > 0. ✓
    (q_mvar, dq_dvm, dq_dvdc)
}

fn dc_bus_power(g_dc: &[Vec<f64>], v_dc: &[f64], dc_bus: usize) -> f64 {
    let i_k: f64 = g_dc[dc_bus]
        .iter()
        .zip(v_dc.iter())
        .map(|(g, v)| g * v)
        .sum();
    i_k * v_dc[dc_bus]
}

#[allow(clippy::too_many_arguments)]
fn evaluate_state(
    network: &Network,
    dc_network: &DcNetwork,
    bus_map: &HashMap<u32, usize>,
    ybus: &YBus,
    g_dc: &[Vec<f64>],
    p_base: &[f64],
    q_base: &[f64],
    stations: &[VscStation],
    lcc_stations: &[LccConverter],
    layout: &SimultaneousLayout,
    x: &[f64],
    droop_p_offsets: &[f64],
) -> Result<SimultaneousEvaluation, HvdcError> {
    let base_mva = network.base_mva;
    let (va, vm, v_dc, q_state) = unpack_state(network, dc_network, layout, stations, x);
    let mut p_spec = p_base.to_vec();
    let mut q_spec = q_base.to_vec();
    let mut p_dc_per_bus = vec![0.0; dc_network.n_buses()];
    let mut station_points: Vec<StationOperatingPoint> = Vec::with_capacity(stations.len());

    // Pre-compute which station is the DC slack: either ConstantVdc or the
    // droop reference (3F). This station's P is set by DC power balance, so
    // we skip it in the main loop and handle it separately below.
    let slack_station_idx = stations
        .iter()
        .position(|s| s.control_mode.is_dc_slack())
        .or_else(|| select_droop_reference(stations).map(|(idx, _, _)| idx));

    for (station_idx, station) in stations.iter().enumerate() {
        let ac_idx = *bus_map
            .get(&station.ac_bus)
            .ok_or(HvdcError::BusNotFound(station.ac_bus))?;
        let v_ac = vm[ac_idx];
        let v_dc_station = v_dc
            .get(station.dc_bus_idx)
            .copied()
            .unwrap_or(dc_network.v_dc_slack);

        if Some(station_idx) == slack_station_idx {
            // DC slack station — P determined by power balance (handled below).
            let q_mvar = match station.control_mode {
                VscHvdcControlMode::ConstantVdc { q_set, .. } => {
                    q_set.clamp(station.q_min_mvar, station.q_max_mvar)
                }
                VscHvdcControlMode::PVdcDroop { .. } => 0.0,
                _ => 0.0,
            };
            station_points.push(StationOperatingPoint {
                net_p_mw: 0.0,
                q_mvar,
                losses_mw: 0.0,
                dnet_dvm_mw: 0.0,
                dnet_dvdc_mw: 0.0,
                dnet_dq_mw_per_mvar: 0.0,
                pvac_constraint: None,
            });
            continue;
        }

        let (q_mvar, pvac_constraint) = match &station.control_mode {
            VscHvdcControlMode::ConstantPQ { q_set, .. } => {
                (q_set.clamp(station.q_min_mvar, station.q_max_mvar), None)
            }
            VscHvdcControlMode::PVdcDroop { .. } => (0.0, None),
            VscHvdcControlMode::ConstantPVac { .. } => {
                let q_mvar = q_state[station_idx];
                let constraint =
                    pvac_constraint(&station.control_mode, v_ac, q_mvar).expect("ConstantPVac");
                (q_mvar, Some(constraint))
            }
            VscHvdcControlMode::ConstantVdc { .. } => unreachable!(),
        };

        let mut p_cmd_mw = station.p_mw(v_dc_station);
        // 3E: apply coordinated droop offset.
        if station.control_mode.is_droop() && station_idx < droop_p_offsets.len() {
            p_cmd_mw += droop_p_offsets[station_idx];
        }
        let dp_dvdc = dp_cmd_dvdc(station, v_dc_station);
        let (losses_mw, dloss_dp, dloss_dq, dloss_dvm) =
            converter_loss_terms(station, p_cmd_mw, q_mvar, v_ac, base_mva);
        let net_p_mw = p_cmd_mw - losses_mw;
        let dnet_dvm_mw = -dloss_dvm;
        let dnet_dvdc_mw = dp_dvdc * (1.0 - dloss_dp);
        let dnet_dq_mw_per_mvar = -dloss_dq;

        p_spec[ac_idx] += net_p_mw / base_mva;
        q_spec[ac_idx] += q_mvar / base_mva;
        p_dc_per_bus[station.dc_bus_idx] += net_p_mw / base_mva;

        station_points.push(StationOperatingPoint {
            net_p_mw,
            q_mvar,
            losses_mw,
            dnet_dvm_mw,
            dnet_dvdc_mw,
            dnet_dq_mw_per_mvar,
            pvac_constraint,
        });
    }

    // ── LCC station contributions (3D) ──────────────────────────────────────
    let mut lcc_points: Vec<LccOperatingPoint> = Vec::with_capacity(lcc_stations.len());
    for lcc in lcc_stations {
        let ac_idx = *bus_map
            .get(&lcc.bus_ac)
            .ok_or(HvdcError::BusNotFound(lcc.bus_ac))?;
        let v_ac = vm[ac_idx];
        let v_dc_lcc = v_dc
            .get(lcc.bus_dc)
            .copied()
            .unwrap_or(dc_network.v_dc_slack);

        // LCC constant-power model: P_dc = p_setpoint_mw.
        // AC injection: opposite sign to DC (rectifier draws from AC).
        let p_ac_mw = -lcc.p_setpoint_mw;

        // Reactive power: Q = −|P_dc| × tan(φ), cos(φ) = V_dc/(K_BRIDGE×V_ac).
        let (q_mvar, dq_dvm, dq_dvdc) = lcc_q_and_derivatives(lcc.p_setpoint_mw, v_dc_lcc, v_ac);

        // AC bus injections.
        p_spec[ac_idx] += p_ac_mw / base_mva;
        q_spec[ac_idx] += q_mvar / base_mva;
        // DC bus injection: power into DC network (positive for rectifier).
        p_dc_per_bus[lcc.bus_dc] += lcc.p_setpoint_mw / base_mva;

        lcc_points.push(LccOperatingPoint {
            _p_ac_mw: p_ac_mw,
            _q_mvar: q_mvar,
            dq_dvm,
            dq_dvdc,
        });
    }

    // ── DC slack station ────────────────────────────────────────────────────
    let slack_station_idx = slack_station_idx
        .expect("validation ensures at least one ConstantVdc or PVdcDroop station");
    let slack_station = &stations[slack_station_idx];
    let slack_dc_bus = dc_network.slack_dc_bus;
    let slack_ac_idx = *bus_map
        .get(&slack_station.ac_bus)
        .ok_or(HvdcError::BusNotFound(slack_station.ac_bus))?;
    let slack_power_needed_pu =
        dc_bus_power(g_dc, &v_dc, slack_dc_bus) - p_dc_per_bus[slack_dc_bus];
    let slack_net_p_mw = slack_power_needed_pu * base_mva;
    let slack_q_mvar = station_points[slack_station_idx].q_mvar;
    let (slack_raw_p_mw, slack_losses_mw) = recover_raw_power_from_net(
        slack_station,
        slack_net_p_mw,
        slack_q_mvar,
        vm[slack_ac_idx],
        base_mva,
    );
    p_spec[slack_ac_idx] += slack_net_p_mw / base_mva;
    q_spec[slack_ac_idx] += slack_q_mvar / base_mva;
    p_dc_per_bus[slack_dc_bus] += slack_net_p_mw / base_mva;
    station_points[slack_station_idx].net_p_mw = slack_net_p_mw;
    station_points[slack_station_idx].losses_mw = slack_losses_mw;
    station_points[slack_station_idx].dnet_dvm_mw = 0.0;
    station_points[slack_station_idx].dnet_dvdc_mw = 0.0;
    station_points[slack_station_idx].dnet_dq_mw_per_mvar = 0.0;
    let _ = slack_raw_p_mw;

    let (p_calc, q_calc) = compute_power_injection(ybus, &vm, &va);
    let mut residual = Vec::with_capacity(layout.n_residual());
    for &bus in &layout.pvpq {
        residual.push(p_spec[bus] - p_calc[bus]);
    }
    for &bus in &layout.pq {
        residual.push(q_spec[bus] - q_calc[bus]);
    }
    for &dc_bus in &layout.dc_free {
        residual.push(dc_bus_power(g_dc, &v_dc, dc_bus) - p_dc_per_bus[dc_bus]);
    }
    for &station_idx in &layout.pvac_stations {
        let station = &stations[station_idx];
        let ac_idx = *bus_map
            .get(&station.ac_bus)
            .ok_or(HvdcError::BusNotFound(station.ac_bus))?;
        let ctrl = station_points[station_idx]
            .pvac_constraint
            .clone()
            .expect("ConstantPVac constraint");
        let q_var = q_state[station_idx];
        match ctrl {
            PvacConstraint::VoltageTarget { target_vm_pu } => {
                residual.push(vm[ac_idx] - target_vm_pu);
            }
            PvacConstraint::ReactiveLimit { target_q_mvar } => {
                residual.push(q_var - target_q_mvar);
            }
        }
    }

    Ok(SimultaneousEvaluation {
        residual_norm: max_abs(&residual),
        residual,
        voltage_magnitude_pu: vm,
        voltage_angle_rad: va,
        v_dc,
        station_points,
        lcc_points,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_analytic_jacobian(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
    ybus: &YBus,
    g_dc: &[Vec<f64>],
    p_base: &[f64],
    q_base: &[f64],
    stations: &[VscStation],
    lcc_stations: &[LccConverter],
    layout: &SimultaneousLayout,
    eval: &SimultaneousEvaluation,
) -> Result<Vec<Vec<f64>>, HvdcError> {
    let base_mva = network.base_mva;
    let dim = layout.n_state();
    let mut jac = vec![vec![0.0; dim]; layout.n_residual()];

    let (p_calc, q_calc) =
        compute_power_injection(ybus, &eval.voltage_magnitude_pu, &eval.voltage_angle_rad);
    let ac_jac_sparse = build_jacobian(
        ybus,
        &eval.voltage_magnitude_pu,
        &eval.voltage_angle_rad,
        &p_calc,
        &q_calc,
        &layout.pvpq,
        &layout.pq,
    );
    let ac_dim = layout.pvpq.len() + layout.pq.len();
    let ac_dense = sparse_to_dense(&ac_jac_sparse, ac_dim);
    for row in 0..ac_dim {
        for col in 0..ac_dim {
            jac[row][col] = -ac_dense[row][col];
        }
    }

    for (station_idx, station) in stations.iter().enumerate() {
        let ac_idx = *bus_map
            .get(&station.ac_bus)
            .ok_or(HvdcError::BusNotFound(station.ac_bus))?;
        let point = &eval.station_points[station_idx];

        if let Some(p_row) = layout.p_row(ac_idx) {
            if let Some(vm_col) = (layout.vm_pos[ac_idx] != NONE).then_some(layout.vm_pos[ac_idx]) {
                jac[p_row][vm_col] += point.dnet_dvm_mw / base_mva;
            }
            if layout.dc_pos[station.dc_bus_idx] != NONE {
                jac[p_row][layout.dc_pos[station.dc_bus_idx]] += point.dnet_dvdc_mw / base_mva;
            }
            if layout.q_pos[station_idx] != NONE {
                jac[p_row][layout.q_pos[station_idx]] += point.dnet_dq_mw_per_mvar / base_mva;
            }
        }

        if let Some(q_row) = layout.q_row(ac_idx)
            && layout.q_pos[station_idx] != NONE
        {
            jac[q_row][layout.q_pos[station_idx]] += 1.0 / base_mva;
        }

        if let Some(dc_row) = layout.dc_row(station.dc_bus_idx) {
            if let Some(vm_col) = (layout.vm_pos[ac_idx] != NONE).then_some(layout.vm_pos[ac_idx]) {
                jac[dc_row][vm_col] -= point.dnet_dvm_mw / base_mva;
            }
            if layout.dc_pos[station.dc_bus_idx] != NONE {
                jac[dc_row][layout.dc_pos[station.dc_bus_idx]] -= point.dnet_dvdc_mw / base_mva;
            }
            if layout.q_pos[station_idx] != NONE {
                jac[dc_row][layout.q_pos[station_idx]] -= point.dnet_dq_mw_per_mvar / base_mva;
            }
        }
    }

    let dc_row_offset = layout.pvpq.len() + layout.pq.len();
    for &dc_bus in &layout.dc_free {
        let row = layout.dc_row(dc_bus).expect("free dc bus row");
        let i_k: f64 = g_dc[dc_bus]
            .iter()
            .zip(eval.v_dc.iter())
            .map(|(g, v)| g * v)
            .sum();
        let _ = dc_row_offset;
        for &dc_bus_m in &layout.dc_free {
            let col = layout.dc_pos[dc_bus_m];
            jac[row][col] += if dc_bus_m == dc_bus {
                i_k + g_dc[dc_bus][dc_bus] * eval.v_dc[dc_bus]
            } else {
                g_dc[dc_bus][dc_bus_m] * eval.v_dc[dc_bus]
            };
        }
    }

    for &station_idx in &layout.pvac_stations {
        let station = &stations[station_idx];
        let ac_idx = *bus_map
            .get(&station.ac_bus)
            .ok_or(HvdcError::BusNotFound(station.ac_bus))?;
        let row = layout
            .q_control_row(station_idx)
            .expect("ConstantPVac control row");
        match eval.station_points[station_idx]
            .pvac_constraint
            .clone()
            .expect("ConstantPVac constraint")
        {
            PvacConstraint::VoltageTarget { .. } => {
                if layout.vm_pos[ac_idx] != NONE {
                    jac[row][layout.vm_pos[ac_idx]] = 1.0;
                }
            }
            PvacConstraint::ReactiveLimit { .. } => {
                jac[row][layout.q_pos[station_idx]] = 1.0;
            }
        }
    }

    // ── LCC Jacobian cross-coupling (3D) ────────────────────────────────────
    // LCC P_ac is constant → no P-row contribution.
    // LCC Q depends on V_ac and V_dc → Q-row cross-coupling.
    // LCC P_dc is constant → no DC-row contribution from LCC directly.
    // (The DC cable Jacobian entries are already handled above.)
    for (lcc_idx, lcc) in lcc_stations.iter().enumerate() {
        let ac_idx = *bus_map
            .get(&lcc.bus_ac)
            .ok_or(HvdcError::BusNotFound(lcc.bus_ac))?;
        let point = &eval.lcc_points[lcc_idx];

        // Q row: dQ_lcc/dV_ac and dQ_lcc/dV_dc.
        if let Some(q_row) = layout.q_row(ac_idx) {
            if layout.vm_pos[ac_idx] != NONE {
                jac[q_row][layout.vm_pos[ac_idx]] += point.dq_dvm / base_mva;
            }
            if lcc.bus_dc < layout.dc_pos.len() && layout.dc_pos[lcc.bus_dc] != NONE {
                jac[q_row][layout.dc_pos[lcc.bus_dc]] += point.dq_dvdc / base_mva;
            }
        }
    }

    let _ = (p_base, q_base);
    Ok(jac)
}

/// Compute coordinated droop power offsets (3E).
///
/// After each Newton iteration, distributes the aggregate DC power imbalance
/// across all PVdcDroop stations proportional to their droop gains.
fn compute_droop_offsets(
    stations: &[VscStation],
    g_dc: &[Vec<f64>],
    v_dc: &[f64],
    p_dc_per_bus_approx: &[f64],
    base_mva: f64,
) -> Vec<f64> {
    let n_dc = v_dc.len();
    // Total DC power imbalance: sum of (cable power - converter injection) over all buses.
    let total_imbalance: f64 = (0..n_dc)
        .map(|k| {
            let i_k: f64 = g_dc[k].iter().zip(v_dc.iter()).map(|(g, v)| g * v).sum();
            i_k * v_dc[k] - p_dc_per_bus_approx[k]
        })
        .sum();

    // Total droop gain.
    let total_k: f64 = stations
        .iter()
        .filter_map(|s| {
            if let VscHvdcControlMode::PVdcDroop { k_droop, .. } = &s.control_mode {
                Some(k_droop.abs())
            } else {
                None
            }
        })
        .sum();

    if total_k < 1e-9 || total_imbalance.abs() < 1e-9 {
        return vec![0.0; stations.len()];
    }

    stations
        .iter()
        .map(|s| {
            if let VscHvdcControlMode::PVdcDroop { k_droop, .. } = &s.control_mode {
                // Distribute imbalance proportionally, scaled to MW.
                -total_imbalance * base_mva * (k_droop.abs() / total_k)
            } else {
                0.0
            }
        })
        .collect()
}

/// Solve an MTDC AC/DC power flow with one simultaneous Newton residual.
///
/// This solver supports VSC stations (`ConstantPQ`, `ConstantPVac`,
/// `ConstantVdc`, `PVdcDroop`), LCC stations (constant-power with analytical
/// firing angle), and all-droop configurations.
pub fn solve_simultaneous_ac_dc(
    network: &Network,
    dc_network: &mut DcNetwork,
    stations: &[VscStation],
    lcc_stations: &[LccConverter],
    opts: &SimultaneousAcDcSolverOptions,
) -> Result<BlockCoupledAcDcResult, HvdcError> {
    info!(
        stations = stations.len(),
        lcc_stations = lcc_stations.len(),
        dc_buses = dc_network.n_buses(),
        max_iter = opts.max_iter,
        tol = opts.tol,
        "Simultaneous AC/DC solve starting"
    );

    let bus_map = network.bus_index_map();
    validate_solver_configuration(network, dc_network, stations, lcc_stations, &bus_map)?;

    // Set DC slack voltage: either from ConstantVdc or from the droop reference.
    if let Some(ref_info) = select_droop_reference(stations) {
        let (ref_idx, v_dc_set, _) = ref_info;
        dc_network.v_dc_slack = v_dc_set;
        dc_network.slack_dc_bus = stations[ref_idx].dc_bus_idx;
        debug!(
            ref_station = ref_idx,
            dc_bus = stations[ref_idx].dc_bus_idx,
            v_dc_set = v_dc_set,
            "All-droop mode: using PVdcDroop station as numerical DC slack"
        );
    } else {
        for station in stations {
            if let VscHvdcControlMode::ConstantVdc { v_dc_target, .. } = station.control_mode {
                dc_network.v_dc_slack = v_dc_target;
            }
        }
    }
    dc_network.v_dc[dc_network.slack_dc_bus] = dc_network.v_dc_slack;

    let base_mva = network.base_mva;
    let layout = SimultaneousLayout::new(network, dc_network, stations);
    let ybus = build_ybus(network);
    let g_dc = dc_network.build_conductance_matrix(dc_network.n_buses());
    let p_base = network.bus_p_injection_pu();
    let q_base = network.bus_q_injection_pu();

    let mut droop_p_offsets = vec![0.0; stations.len()];
    let mut x = warm_start_from_block_solution(network, dc_network, &layout, stations, opts)
        .unwrap_or_else(|| initial_state_vector(network, dc_network, &layout));
    let mut last_eval = evaluate_state(
        network,
        dc_network,
        &bus_map,
        &ybus,
        &g_dc,
        &p_base,
        &q_base,
        stations,
        lcc_stations,
        &layout,
        &x,
        &droop_p_offsets,
    )?;
    let mut converged = false;
    let mut iterations = 0;

    for iter in 0..opts.max_iter {
        iterations = iter + 1;
        debug!(
            iteration = iterations,
            residual = last_eval.residual_norm,
            "Simultaneous AC/DC Newton iteration"
        );

        if last_eval.residual_norm < opts.tol {
            converged = true;
            break;
        }

        // 3E: coordinated droop adjustment between iterations.
        if opts.coordinated_droop {
            let mut p_dc_approx = vec![0.0; dc_network.n_buses()];
            for (i, pt) in last_eval.station_points.iter().enumerate() {
                p_dc_approx[stations[i].dc_bus_idx] += pt.net_p_mw / base_mva;
            }
            for (i, lcc) in lcc_stations.iter().enumerate() {
                let _ = &last_eval.lcc_points[i];
                p_dc_approx[lcc.bus_dc] += lcc.p_setpoint_mw / base_mva;
            }
            droop_p_offsets =
                compute_droop_offsets(stations, &g_dc, &last_eval.v_dc, &p_dc_approx, base_mva);
        }

        let jac = build_analytic_jacobian(
            network,
            &bus_map,
            &ybus,
            &g_dc,
            &p_base,
            &q_base,
            stations,
            lcc_stations,
            &layout,
            &last_eval,
        )?;
        let rhs: Vec<f64> = last_eval.residual.iter().map(|v| -v).collect();
        let delta = solve_system(&jac, &rhs);
        if max_abs(&delta) < 1e-12 {
            break;
        }

        let mut accepted = None;
        for ls in 0..=opts.max_line_search_steps {
            let alpha = 0.5f64.powi(ls as i32);
            let trial_x: Vec<f64> = x
                .iter()
                .zip(delta.iter())
                .map(|(xi, dxi)| xi + alpha * dxi)
                .collect();
            let trial_eval = evaluate_state(
                network,
                dc_network,
                &bus_map,
                &ybus,
                &g_dc,
                &p_base,
                &q_base,
                stations,
                lcc_stations,
                &layout,
                &trial_x,
                &droop_p_offsets,
            )?;
            if trial_eval.residual_norm < last_eval.residual_norm {
                accepted = Some((trial_x, trial_eval));
                break;
            }
            if accepted
                .as_ref()
                .map(|(_, eval): &(Vec<f64>, SimultaneousEvaluation)| {
                    trial_eval.residual_norm < eval.residual_norm
                })
                .unwrap_or(true)
            {
                accepted = Some((trial_x, trial_eval));
            }
        }

        if let Some((trial_x, trial_eval)) = accepted
            && trial_eval.residual_norm < last_eval.residual_norm
        {
            x = trial_x;
            last_eval = trial_eval;
            continue;
        }
        break;
    }

    dc_network.v_dc = last_eval.v_dc.clone();
    dc_network.v_dc[dc_network.slack_dc_bus] = dc_network.v_dc_slack;
    let (branch_flows, branch_losses) = dc_network.compute_branch_flows();
    let (shunt_losses, ground_losses) = dc_network.compute_shunt_ground_losses();
    let dc_result = DcPfResult {
        v_dc: dc_network.v_dc.clone(),
        branch_flows,
        branch_losses,
        shunt_losses,
        ground_losses,
        converged,
        iterations,
    };

    let station_results: Vec<VscStationResult> = stations
        .iter()
        .enumerate()
        .map(|(i, station)| VscStationResult {
            ac_bus: station.ac_bus,
            dc_bus_idx: station.dc_bus_idx,
            p_ac_mw: last_eval.station_points[i].net_p_mw,
            q_ac_mvar: last_eval.station_points[i].q_mvar,
            p_dc_mw: if last_eval.station_points[i].net_p_mw >= 0.0 {
                last_eval.station_points[i].net_p_mw + last_eval.station_points[i].losses_mw
            } else {
                (-last_eval.station_points[i].net_p_mw - last_eval.station_points[i].losses_mw)
                    .max(0.0)
            },
            v_dc_pu: last_eval
                .v_dc
                .get(station.dc_bus_idx)
                .copied()
                .unwrap_or(dc_network.v_dc_slack),
            losses_mw: last_eval.station_points[i].losses_mw,
        })
        .collect();

    if !converged {
        warn!(
            iterations = iterations,
            residual = last_eval.residual_norm,
            "Simultaneous AC/DC solve did not converge"
        );
    }

    info!(
        iterations = iterations,
        converged = converged,
        residual = last_eval.residual_norm,
        "Simultaneous AC/DC solve complete"
    );

    Ok(BlockCoupledAcDcResult {
        dc_result,
        station_results,
        iterations,
        converged,
        v_ac: last_eval.voltage_magnitude_pu,
        theta_ac: last_eval.voltage_angle_rad,
        bus_numbers: network.buses.iter().map(|b| b.number).collect(),
        method: AcDcMethod::Simultaneous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dc_network::topology::DcCable;
    use crate::solver::block_coupled::{BlockCoupledAcDcSolverOptions, solve_block_coupled_ac_dc};
    use surge_network::network::{Generator, Load};

    fn build_test_ac_dc_system() -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("simultaneous-acdc-test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.02;
        b1.voltage_angle_rad = 0.0;
        net.buses.push(b1);

        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 10.0));

        let b3 = Bus::new(3, BusType::PQ, 230.0);
        net.buses.push(b3);
        net.loads.push(Load::new(3, 20.0, 0.0));

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.02));

        let mut gobj = Generator::new(1, 200.0, 1.02);
        gobj.pmax = 500.0;
        gobj.qmax = 300.0;
        gobj.qmin = -300.0;
        net.generators.push(gobj);

        let mut dc_net = DcNetwork::new(2, 0);
        dc_net.v_dc_slack = 1.0;
        dc_net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.05,
            i_max_pu: 5.0,
        });

        let s0 = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantVdc {
                v_dc_target: 1.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        let s1 = VscStation {
            ac_bus: 2,
            dc_bus_idx: 1,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 28.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.005,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };

        (net, dc_net, vec![s0, s1])
    }

    #[test]
    fn simultaneous_solver_converges_on_small_mtdc_case() {
        let (net, mut dc_net, stations) = build_test_ac_dc_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 20,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &[], &opts)
            .expect("simultaneous solver should succeed");
        assert!(result.converged, "simultaneous solver should converge");
        assert_eq!(result.method, AcDcMethod::Simultaneous);
        assert_eq!(result.dc_result.v_dc.len(), 2);
        assert!(
            result.station_results[0].p_ac_mw.abs() > 1e-6,
            "ConstantVdc slack station should carry AC-side active power"
        );
    }

    #[test]
    fn simultaneous_and_block_coupled_agree_on_small_case() {
        let (net, mut dc_net_sim, stations) = build_test_ac_dc_system();
        let (_, mut dc_net_block, _) = build_test_ac_dc_system();

        let sim_opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 20,
            ..Default::default()
        };
        let block_opts = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..Default::default()
        };

        let sim = solve_simultaneous_ac_dc(&net, &mut dc_net_sim, &stations, &[], &sim_opts)
            .expect("simultaneous solve should succeed");
        let block = solve_block_coupled_ac_dc(&net, &mut dc_net_block, &stations, &block_opts)
            .expect("block-coupled solve should succeed");

        assert!(sim.converged);
        assert!(block.converged);
        for (lhs, rhs) in sim.v_ac.iter().zip(block.v_ac.iter()) {
            assert!((lhs - rhs).abs() < 1e-4, "v_ac mismatch: {lhs} vs {rhs}");
        }
        for (lhs, rhs) in sim.dc_result.v_dc.iter().zip(block.dc_result.v_dc.iter()) {
            assert!((lhs - rhs).abs() < 1e-4, "v_dc mismatch: {lhs} vs {rhs}");
        }
    }

    #[test]
    fn simultaneous_supports_constant_pvac() {
        let (net, mut dc_net, mut stations) = build_test_ac_dc_system();
        stations[1].control_mode = VscHvdcControlMode::ConstantPVac {
            p_set: 28.0,
            v_target: 1.03,
            v_band: 0.01,
            q_min: -50.0,
            q_max: 50.0,
        };

        let result = solve_simultaneous_ac_dc(
            &net,
            &mut dc_net,
            &stations,
            &[],
            &SimultaneousAcDcSolverOptions::default(),
        )
        .expect("ConstantPVac should be supported");

        assert!(result.converged);
        assert!(
            result.station_results[1].q_ac_mvar > 0.0,
            "PVac station should inject positive Q to support low voltage"
        );
        assert!(result.station_results[1].q_ac_mvar <= 50.0);
    }

    #[test]
    fn simultaneous_requires_dc_voltage_control() {
        let (net, mut dc_net, mut stations) = build_test_ac_dc_system();
        // Set both stations to ConstantPQ → no DC voltage reference.
        stations[0].control_mode = VscHvdcControlMode::ConstantPQ {
            p_set: 0.0,
            q_set: 0.0,
        };

        let err = solve_simultaneous_ac_dc(
            &net,
            &mut dc_net,
            &stations,
            &[],
            &SimultaneousAcDcSolverOptions::default(),
        )
        .expect_err("DC voltage control should be required");

        assert!(matches!(err, HvdcError::UnsupportedConfiguration(_)));
    }

    // ── 3D: LCC support tests ────────────────────────────────────────────────

    fn build_lcc_test_system() -> (Network, DcNetwork, Vec<VscStation>, Vec<LccConverter>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("lcc-simultaneous-test");
        net.base_mva = 100.0;

        // 4-bus AC: slack + 3 PQ buses.
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.02;
        net.buses.push(b1);
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 30.0, 10.0));
        let b3 = Bus::new(3, BusType::PQ, 230.0);
        net.buses.push(b3);
        net.loads.push(Load::new(3, 40.0, 15.0));
        let b4 = Bus::new(4, BusType::PQ, 230.0);
        net.buses.push(b4);
        net.loads.push(Load::new(4, 20.0, 0.0));

        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(3, 4, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(1, 4, 0.01, 0.05, 0.02));

        let mut g1 = Generator::new(1, 300.0, 1.02);
        g1.pmax = 500.0;
        g1.qmax = 300.0;
        g1.qmin = -300.0;
        net.generators.push(g1);

        // 3-bus DC: bus 0 (slack), bus 1 (LCC rectifier), bus 2 (VSC inverter 1),
        // DC bus 3 not needed — we use 3 DC buses.
        let mut dc_net = DcNetwork::new(3, 0);
        dc_net.v_dc_slack = 1.0;
        dc_net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.01,
            i_max_pu: 10.0,
        });
        dc_net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 2,
            r_dc_pu: 0.01,
            i_max_pu: 10.0,
        });
        dc_net.add_cable(DcCable {
            from_dc_bus: 1,
            to_dc_bus: 2,
            r_dc_pu: 0.01,
            i_max_pu: 10.0,
        });

        // VSC ConstantVdc slack at DC bus 0, AC bus 1.
        let vsc_slack = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantVdc {
                v_dc_target: 1.0,
                q_set: 0.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        // VSC inverter at DC bus 2, AC bus 3.
        let vsc_inv = VscStation {
            ac_bus: 3,
            dc_bus_idx: 2,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 20.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        let stations = vec![vsc_slack, vsc_inv];

        // LCC rectifier at DC bus 1, AC bus 2: 30 MW into DC.
        let mut lcc = LccConverter::new(2, 1, 30.0);
        lcc.p_setpoint_mw = 30.0;
        lcc.x_commutation_pu = 0.15;
        let lcc_stations = vec![lcc];

        (net, dc_net, stations, lcc_stations)
    }

    #[test]
    fn simultaneous_lcc_converges() {
        let (net, mut dc_net, stations, lcc_stations) = build_lcc_test_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &lcc_stations, &opts)
            .expect("LCC+VSC simultaneous solver should succeed");
        assert!(
            result.converged,
            "LCC+VSC simultaneous solver should converge"
        );
        assert_eq!(result.method, AcDcMethod::Simultaneous);
    }

    #[test]
    fn simultaneous_lcc_q_absorption() {
        let (net, mut dc_net, stations, lcc_stations) = build_lcc_test_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &lcc_stations, &opts)
            .expect("LCC+VSC solve should succeed");

        // The LCC bus (AC bus 2) should have additional Q absorption.
        // We can verify by checking the AC bus voltage at bus 2 — it should
        // be slightly lower due to the LCC Q absorption.
        assert!(result.converged);
        // All DC voltages should be realistic.
        for v in &result.dc_result.v_dc {
            assert!(*v > 0.85 && *v < 1.15, "DC voltage {v:.4} out of range");
        }
    }

    #[test]
    fn simultaneous_lcc_dc_bus_voltages_realistic() {
        let (net, mut dc_net, stations, lcc_stations) = build_lcc_test_system();
        let result = solve_simultaneous_ac_dc(
            &net,
            &mut dc_net,
            &stations,
            &lcc_stations,
            &SimultaneousAcDcSolverOptions::default(),
        )
        .expect("solve should succeed");

        assert!(result.converged);
        for (i, v) in result.dc_result.v_dc.iter().enumerate() {
            assert!(
                *v > 0.8 && *v < 1.2,
                "DC bus {i} voltage {v:.4} out of range"
            );
        }
    }

    // ── 3E: Coordinated droop tests ──────────────────────────────────────────

    fn build_4station_droop_system() -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("droop-coordination-test");
        net.base_mva = 100.0;

        // 5-bus AC: 1 slack + 4 PQ.
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.02;
        net.buses.push(b1);
        for i in 2..=5u32 {
            let b = Bus::new(i, BusType::PQ, 230.0);
            net.buses.push(b);
            net.loads.push(Load::new(i, 20.0, 5.0));
        }
        for (f, t) in [(1, 2), (2, 3), (3, 4), (4, 5), (1, 5)] {
            net.branches.push(Branch::new_line(f, t, 0.01, 0.05, 0.02));
        }
        let mut g1 = Generator::new(1, 500.0, 1.02);
        g1.pmax = 600.0;
        g1.qmax = 300.0;
        g1.qmin = -300.0;
        net.generators.push(g1);

        // 4-bus DC with ConstantVdc slack + 3 droop stations.
        let mut dc_net = DcNetwork::new(4, 0);
        dc_net.v_dc_slack = 1.0;
        for (f, t) in [(0, 1), (1, 2), (2, 3), (0, 3)] {
            dc_net.add_cable(DcCable {
                from_dc_bus: f,
                to_dc_bus: t,
                r_dc_pu: 0.02,
                i_max_pu: 10.0,
            });
        }

        let vsc0 = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantVdc {
                v_dc_target: 1.0,
                q_set: 0.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        let make_droop = |ac_bus: u32, dc_bus: usize, k: f64| VscStation {
            ac_bus,
            dc_bus_idx: dc_bus,
            control_mode: VscHvdcControlMode::PVdcDroop {
                p_set: 15.0,
                voltage_dc_setpoint_pu: 1.0,
                k_droop: k,
                p_min: -50.0,
                p_max: 50.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        let stations = vec![
            vsc0,
            make_droop(3, 1, 100.0),
            make_droop(4, 2, 200.0),
            make_droop(5, 3, 50.0),
        ];

        (net, dc_net, stations)
    }

    #[test]
    fn coordinated_droop_converges() {
        let (net, mut dc_net, stations) = build_4station_droop_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            coordinated_droop: true,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &[], &opts)
            .expect("coordinated droop should succeed");
        assert!(result.converged, "coordinated droop should converge");
    }

    #[test]
    fn coordinated_droop_shares_load_proportionally() {
        let (net, mut dc_net, stations) = build_4station_droop_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            coordinated_droop: true,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &[], &opts)
            .expect("coordinated droop should succeed");
        assert!(result.converged);

        // Droop stations 1,2,3 have gains 100, 200, 50 MW/pu.
        // At convergence their P should be near their setpoints (since V_dc ≈ 1.0).
        for sr in &result.station_results[1..] {
            assert!(
                sr.p_ac_mw.abs() < 80.0,
                "droop station P={:.1} MW out of expected range",
                sr.p_ac_mw
            );
        }
    }

    // ── 3F: All-droop (no ConstantVdc) tests ─────────────────────────────────

    fn build_all_droop_system() -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("all-droop-test");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.02;
        net.buses.push(b1);
        for i in 2..=4u32 {
            let b = Bus::new(i, BusType::PQ, 230.0);
            net.buses.push(b);
            net.loads.push(Load::new(i, 25.0, 5.0));
        }
        for (f, t) in [(1, 2), (2, 3), (3, 4), (1, 4)] {
            net.branches.push(Branch::new_line(f, t, 0.01, 0.05, 0.02));
        }
        let mut g1 = Generator::new(1, 400.0, 1.02);
        g1.pmax = 500.0;
        g1.qmax = 300.0;
        g1.qmin = -300.0;
        net.generators.push(g1);

        // 3-bus DC, ALL droop (no ConstantVdc).
        let mut dc_net = DcNetwork::new(3, 0);
        dc_net.v_dc_slack = 1.0;
        for (f, t) in [(0, 1), (1, 2), (0, 2)] {
            dc_net.add_cable(DcCable {
                from_dc_bus: f,
                to_dc_bus: t,
                r_dc_pu: 0.02,
                i_max_pu: 10.0,
            });
        }

        let make_droop = |ac_bus: u32, dc_bus: usize, k: f64, p: f64| VscStation {
            ac_bus,
            dc_bus_idx: dc_bus,
            control_mode: VscHvdcControlMode::PVdcDroop {
                p_set: p,
                voltage_dc_setpoint_pu: 1.0,
                k_droop: k,
                p_min: -80.0,
                p_max: 80.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };

        let stations = vec![
            make_droop(1, 0, 300.0, 10.0), // largest gain → reference
            make_droop(2, 1, 100.0, 15.0),
            make_droop(3, 2, 50.0, 20.0),
        ];

        (net, dc_net, stations)
    }

    #[test]
    fn all_droop_converges() {
        let (net, mut dc_net, stations) = build_all_droop_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &[], &opts)
            .expect("all-droop (no ConstantVdc) should succeed");
        assert!(result.converged, "all-droop system should converge");
    }

    #[test]
    fn all_droop_vdc_near_setpoints() {
        let (net, mut dc_net, stations) = build_all_droop_system();
        let opts = SimultaneousAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..Default::default()
        };

        let result = solve_simultaneous_ac_dc(&net, &mut dc_net, &stations, &[], &opts)
            .expect("all-droop solve should succeed");
        assert!(result.converged);

        // DC voltages should stay near 1.0 pu setpoints.
        for (i, v) in result.dc_result.v_dc.iter().enumerate() {
            assert!(
                (*v - 1.0).abs() < 0.15,
                "DC bus {i} voltage {v:.4} too far from 1.0 pu setpoint"
            );
        }
    }

    #[test]
    fn all_droop_rejects_no_dc_control() {
        let (net, mut dc_net, _) = build_all_droop_system();
        // All ConstantPQ → no DC voltage reference at all.
        let stations = vec![VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 10.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        }];

        let err = solve_simultaneous_ac_dc(
            &net,
            &mut dc_net,
            &stations,
            &[],
            &SimultaneousAcDcSolverOptions::default(),
        )
        .expect_err("no DC voltage control should be rejected");
        assert!(matches!(err, HvdcError::UnsupportedConfiguration(_)));
    }

    #[test]
    fn recover_raw_power_from_net_matches_station_loss_equation() {
        let station = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 0.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.01,
            loss_linear: 0.02,
            loss_c_rectifier: 0.015,
            loss_c_inverter: 0.015,
        };

        let (raw_p_mw, losses_mw) = recover_raw_power_from_net(&station, 80.0, 12.0, 0.97, 100.0);
        let expected_losses = station.losses_mw(raw_p_mw, 12.0, 0.97, 100.0);

        assert!((losses_mw - expected_losses).abs() < 1e-9);
        assert!((raw_p_mw - (80.0 + expected_losses)).abs() < 1e-8);
        assert!(raw_p_mw > 80.0);
    }

    #[test]
    fn compute_droop_offsets_distributes_imbalance_by_absolute_gain() {
        let stations = vec![
            VscStation {
                ac_bus: 1,
                dc_bus_idx: 0,
                control_mode: VscHvdcControlMode::ConstantVdc {
                    v_dc_target: 1.0,
                    q_set: 0.0,
                },
                q_max_mvar: 50.0,
                q_min_mvar: -50.0,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                loss_c_rectifier: 0.0,
                loss_c_inverter: 0.0,
            },
            VscStation {
                ac_bus: 2,
                dc_bus_idx: 1,
                control_mode: VscHvdcControlMode::PVdcDroop {
                    p_set: 20.0,
                    voltage_dc_setpoint_pu: 1.0,
                    k_droop: 10.0,
                    p_min: 0.0,
                    p_max: 100.0,
                },
                q_max_mvar: 50.0,
                q_min_mvar: -50.0,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                loss_c_rectifier: 0.0,
                loss_c_inverter: 0.0,
            },
            VscStation {
                ac_bus: 3,
                dc_bus_idx: 2,
                control_mode: VscHvdcControlMode::PVdcDroop {
                    p_set: 20.0,
                    voltage_dc_setpoint_pu: 1.0,
                    k_droop: 30.0,
                    p_min: 0.0,
                    p_max: 100.0,
                },
                q_max_mvar: 50.0,
                q_min_mvar: -50.0,
                loss_constant_mw: 0.0,
                loss_linear: 0.0,
                loss_c_rectifier: 0.0,
                loss_c_inverter: 0.0,
            },
        ];
        let g_dc = vec![
            vec![1.0, -1.0, 0.0],
            vec![-1.0, 2.0, -1.0],
            vec![0.0, -1.0, 1.0],
        ];
        let v_dc = vec![1.0, 0.95, 0.9];
        let p_dc_per_bus = vec![0.03, 0.06, -0.01];

        let offsets = compute_droop_offsets(&stations, &g_dc, &v_dc, &p_dc_per_bus, 100.0);

        let total_offset: f64 = offsets.iter().sum();
        let total_imbalance: f64 = (0..v_dc.len())
            .map(|k| {
                let i_k: f64 = g_dc[k].iter().zip(v_dc.iter()).map(|(g, v)| g * v).sum();
                i_k * v_dc[k] - p_dc_per_bus[k]
            })
            .sum();

        assert!(offsets[0].abs() < 1e-12);
        assert!((total_offset + total_imbalance * 100.0).abs() < 1e-9);
        assert!((offsets[1] / offsets[2] - (1.0 / 3.0)).abs() < 1e-9);
    }
}
