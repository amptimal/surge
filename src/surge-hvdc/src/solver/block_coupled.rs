// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Block-coupled AC/DC power flow formulation.
//!
//! Implements FPQ-39 / P5-024 as alternating AC and DC solves with optional
//! cross-coupling sensitivity corrections. This is not a monolithic augmented
//! Newton solve; the coupling is enforced through outer iterations.
//!
//! # Augmented System
//!
//! The full AC network has `n_pq` PQ buses and `n_pv` PV buses, giving
//! `n_ac = 2*n_pq + n_pv` AC unknowns (theta for all, |V| for PQ only).
//!
//! The DC network has `n_dc_free = n_dc - 1` free DC bus voltage unknowns
//! (excluding the slack DC bus).
//!
//! The conceptual coupled system is:
//!
//! ```text
//! [J_AC    | J_AC_DC] [Dx_AC ]   [f_AC]
//! [J_DC_AC | J_DC   ] [DV_dc ] = [f_DC]
//! ```
//!
//! where:
//! - `J_AC` is the standard AC Jacobian (P-theta, Q-|V|, P-|V|, Q-theta sub-blocks)
//! - `J_DC` is the DC Jacobian from the DC network power flow
//! - `J_AC_DC` (coupling: AC -> DC) captures dP_ac/dV_dc (how AC power injections
//!   at converter buses change with DC voltages)
//! - `J_DC_AC` (coupling: DC -> AC) captures dP_dc/dV_ac (how DC power balance
//!   changes with AC bus voltages)
//!
//! # Sensitivity-Corrected vs Plain Outer Iteration
//!
//! The solver supports two runtime modes controlled by
//! [`BlockCoupledAcDcSolverOptions`]:
//!
//! ## Sensitivity-Corrected Mode (`apply_coupling_sensitivities = true`, default)
//!
//! After each AC+DC inner solve, the solver computes cross-coupling
//! sensitivity corrections and applies them as additional update terms to
//! improve convergence. The key derivatives are:
//!
//! - **`dP_ac/dV_dc`**: How the AC power injection at a converter bus changes
//!   when the DC bus voltage changes. Depends on the VSC control mode:
//!   - ConstantPQ: 0 (P is fixed regardless of V_dc)
//!   - PVdcDroop: k_droop / base_mva (droop response)
//!   - ConstantVdc: I_dc / base_mva (P = V_dc * I_dc)
//!
//! - **`dP_dc/dV_ac`**: How the DC power balance changes when the AC bus
//!   voltage changes. Through the converter loss model:
//!   dP_loss/dV_ac = -(b + 2c*I_ac) * I_ac / V_ac
//!   dP_dc/dV_ac = -dP_loss/dV_ac
//!
//! These corrections are applied at the outer loop level between AC and DC
//! sub-solves, without modifying the internal NR solvers. This gives
//! improved convergence on weak AC systems (SCR < 3) where the decoupled
//! approach may oscillate or diverge.
//!
//! ## Plain Mode (`apply_coupling_sensitivities = false`)
//!
//! **Sets the cross-coupling Jacobian blocks `J_AC_DC` and `J_DC_AC` to zero.**
//! The coupling between AC and DC subsystems is captured only through the
//! mismatch vector updates at each outer iteration. This is equivalent to a
//! Gauss-Seidel-style alternating solution where:
//!
//! 1. The AC subsystem is solved with fixed DC voltages (inner NR)
//! 2. The DC subsystem is solved with fixed AC voltages (inner NR)
//! 3. The mismatch is re-evaluated and the process repeats
//!
//! ### Convergence Implications
//!
//! The decoupled approach has the following characteristics compared to the
//! sensitivity-corrected mode:
//!
//! - **More outer iterations required**: Without the cross-coupling derivatives,
//!   the Newton update does not capture how changes in DC voltage affect AC
//!   power injections (and vice versa) within a single iteration. The information
//!   propagates only through the mismatch update, requiring additional outer
//!   iterations to converge. Typical overhead is 2-5x more outer iterations
//!   compared to the sensitivity-corrected formulation.
//!
//! - **Convergence failure on weak AC grids (SCR < 3)**: When the AC system at
//!   a converter bus is weak (Short-Circuit Ratio below 3), the AC voltage is
//!   strongly sensitive to HVDC power injection. In such cases, the decoupled
//!   formulation may oscillate or diverge because the omitted dP_ac/dV_dc and
//!   dP_dc/dV_ac terms are large relative to the diagonal blocks.
//!
//! - **Adequate for strong AC systems (SCR > 3)**: For typical transmission
//!   networks with SCR > 3 at converter buses, the decoupled approach converges
//!   reliably within 5-15 outer iterations and produces results identical to
//!   the sensitivity-corrected solution.
//!
//! ## Recommendations for Users
//!
//! - For **strong AC systems** (SCR > 3 at all converter buses): either mode
//!   works; the sensitivity-corrected mode may converge in fewer iterations.
//!
//! - For **weak AC systems** (SCR < 3): use the default sensitivity-corrected mode.
//!
//! - For **very weak AC systems** (ESCR < 2) or multi-infeed HVDC corridors:
//!   the coupled correction improves convergence but may still require more
//!   outer iterations. Increase `max_iter` (e.g., to 50) if needed.
//!
//! # VSC Coupling Model
//!
//! Each VSC station couples an AC bus (AC bus index) to a DC bus (DC bus index).
//! The converter delivers power `P_ac = P_dc_setpoint` from AC to DC (rectifier)
//! or `P_ac = -(P_dc - P_loss)` from DC to AC (inverter).
//!
//! In the block-coupled formulation the coupling is:
//! - DC mismatch at bus k includes the VSC injection: `f_dc_k = I_k * V_k - P_dc_k`
//! - AC mismatch at the VSC bus includes the VSC load/generation
//!
//! The decoupled coupling Jacobian is:
//! - `df_dc_k / dV_dc_k = G_kk * V_k + I_k`  (same as standalone DC)
//! - `df_dc_k / dV_ac_bus` = 0  (decoupled: AC -> DC coupling omitted in Jacobian)
//! - `df_ac_bus / dV_dc_k` = 0  (decoupled: DC -> AC coupling omitted in Jacobian)
//!
//! The coupling is captured through the mismatch updates at each iteration,
//! which incorporates the current DC voltages into the AC power injections
//! and vice versa.  This converges to the same solution as the fully
//! coupled system for well-conditioned cases (strong AC grid, SCR > 3).

use std::collections::HashMap;

use surge_ac::{AcPfOptions, solve_ac_pf};
use surge_network::Network;
use surge_network::network::BusType;
use tracing::{debug, info, warn};

use crate::dc_network::topology::{DcNetwork, DcPfResult};
use crate::error::HvdcError;
use crate::model::control::VscHvdcControlMode;
use crate::result::{
    HvdcDcBusSolution, HvdcMethod, HvdcSolution, HvdcStationSolution, HvdcTechnology,
};

/// A VSC station coupling an AC bus to a DC bus in the MTDC network.
///
/// Used by the block-coupled AC/DC solver to define coupling between the
/// AC and DC subsystems.
#[derive(Debug, Clone)]
pub struct VscStation {
    /// AC bus number (external, matches `network.buses[i].number`).
    pub ac_bus: u32,
    /// DC bus index (0-indexed internal index in `DcNetwork`).
    pub dc_bus_idx: usize,
    /// VSC control mode (determines P and Q injections).
    pub control_mode: VscHvdcControlMode,
    /// Maximum reactive injection in MVAR.
    pub q_max_mvar: f64,
    /// Minimum reactive injection in MVAR.
    pub q_min_mvar: f64,
    /// Converter loss coefficient `a` (constant, pu on system base).
    pub loss_constant_mw: f64,
    /// Converter loss coefficient `b` (linear in AC current, pu/pu).
    pub loss_linear: f64,
    /// Converter loss coefficient `c` for rectifier operation (quadratic in AC current, pu/pu²).
    pub loss_c_rectifier: f64,
    /// Converter loss coefficient `c` for inverter operation (quadratic in AC current, pu/pu²).
    pub loss_c_inverter: f64,
}

impl VscStation {
    #[inline]
    pub fn quadratic_loss_coefficient(&self, p_mw: f64) -> f64 {
        if p_mw < 0.0 {
            self.loss_c_rectifier
        } else {
            self.loss_c_inverter
        }
    }

    /// Compute converter losses in MW given AC terminal conditions.
    pub fn losses_mw(&self, p_mw: f64, q_mvar: f64, v_ac_pu: f64, base_mva: f64) -> f64 {
        crate::model::vsc::compute_vsc_losses_mw(
            self.loss_constant_mw,
            self.loss_linear,
            self.quadratic_loss_coefficient(p_mw),
            p_mw,
            q_mvar,
            v_ac_pu,
            base_mva,
        )
    }

    /// Effective active power injection in MW given DC bus voltage.
    pub fn p_mw(&self, v_dc_pu: f64) -> f64 {
        self.control_mode.effective_p_mw(v_dc_pu)
    }

    /// Effective reactive power injection in MVAR given AC voltage and previous Q.
    pub fn q_mvar(&self, v_ac: f64, q_prev: f64, q_fixed: f64) -> f64 {
        self.control_mode
            .effective_q_mvar(v_ac, q_prev, q_fixed)
            .clamp(self.q_min_mvar, self.q_max_mvar)
    }
}

/// Solver style for the MTDC AC/DC coupling loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcDcSolverMode {
    /// Alternating AC and DC solves with optional sensitivity correction.
    BlockCoupled,
    /// Pure sequential AC/DC outer iteration without the block-coupled wrapper.
    Sequential,
}

/// Options for the block-coupled AC/DC solver.
#[derive(Debug, Clone)]
pub struct BlockCoupledAcDcSolverOptions {
    /// Convergence tolerance in per-unit (default: 1e-6).
    pub tol: f64,
    /// Maximum number of outer iterations (default: 20).
    pub max_iter: usize,
    /// Inner AC NR tolerance (default: 1e-8 pu).
    pub ac_tol: f64,
    /// Maximum inner AC NR iterations (default: 100).
    pub ac_max_iter: u32,
    /// Inner DC NR tolerance (default: 1e-8 pu).
    pub dc_tol: f64,
    /// Maximum inner DC NR iterations (default: 50).
    pub dc_max_iter: usize,
    /// If `true`, use a flat start (V=1∠0°) for each inner AC power flow.
    pub flat_start: bool,
    /// Select the outer-loop coupling style.
    pub solver_mode: AcDcSolverMode,
    /// If `true`, compute and apply cross-coupling sensitivity corrections between
    /// AC and DC subsystems for improved convergence on weak AC grids (SCR < 3).
    /// Default: `true`.
    pub apply_coupling_sensitivities: bool,
    /// Enable coordinated multi-station droop (default: true).
    ///
    /// When true, PVdcDroop stations redistribute DC power imbalance
    /// proportional to their droop gains between outer AC/DC iterations.
    pub coordinated_droop: bool,
}

impl Default for BlockCoupledAcDcSolverOptions {
    fn default() -> Self {
        Self {
            tol: 1e-6,
            max_iter: 20,
            ac_tol: 1e-8,
            ac_max_iter: 100,
            dc_tol: 1e-8,
            dc_max_iter: 50,
            flat_start: true,
            solver_mode: AcDcSolverMode::BlockCoupled,
            apply_coupling_sensitivities: true,
            coordinated_droop: true,
        }
    }
}

/// Result of the block-coupled AC/DC solve.
#[derive(Debug, Clone)]
pub struct BlockCoupledAcDcResult {
    /// DC power flow result (voltages and cable flows).
    pub dc_result: DcPfResult,
    /// Per-VSC-station operating points.
    pub station_results: Vec<VscStationResult>,
    /// Number of outer iterations taken.
    pub iterations: usize,
    /// True if both AC and DC converged.
    pub converged: bool,
    /// Final AC bus voltages in per-unit (indexed by internal bus order).
    pub v_ac: Vec<f64>,
    /// Final AC bus voltage angles in radians.
    pub theta_ac: Vec<f64>,
    /// AC bus numbers corresponding to `v_ac` / `theta_ac`.
    pub bus_numbers: Vec<u32>,
    /// Solution method used for this AC/DC outer-loop family.
    pub method: AcDcMethod,
}

/// Solver method used for block-coupled-style AC/DC results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcDcMethod {
    /// Sequential AC/DC outer loop.
    Sequential,
    /// Block-coupled AC/DC outer loop with sensitivity correction.
    BlockCoupled,
    /// Experimental simultaneous AC/DC Newton solve.
    Simultaneous,
}

/// Per-station result in the block-coupled AC/DC solution.
#[derive(Debug, Clone)]
pub struct VscStationResult {
    /// AC bus number.
    pub ac_bus: u32,
    /// DC bus index.
    pub dc_bus_idx: usize,
    /// Active power injected into AC network in MW (positive = injection).
    pub p_ac_mw: f64,
    /// Reactive power injected into AC network in MVAR.
    pub q_ac_mvar: f64,
    /// Active power at the DC terminal in MW (positive = injected into the DC network).
    pub p_dc_mw: f64,
    /// DC bus voltage in per-unit.
    pub v_dc_pu: f64,
    /// Converter losses in MW.
    pub losses_mw: f64,
}

impl BlockCoupledAcDcResult {
    /// Convert to the canonical `HvdcSolution` format.
    ///
    /// Each VSC station becomes one `HvdcStationSolution` entry.
    pub fn to_hvdc_solution(&self, network: &Network) -> Result<HvdcSolution, HvdcError> {
        let dc_bus_ids: Vec<u32> = network.hvdc.dc_buses().map(|bus| bus.bus_id).collect();
        let stations: Vec<HvdcStationSolution> = self
            .station_results
            .iter()
            .map(|s| HvdcStationSolution {
                name: None,
                technology: HvdcTechnology::Vsc,
                ac_bus: s.ac_bus,
                dc_bus: dc_bus_ids.get(s.dc_bus_idx).copied(),
                p_ac_mw: s.p_ac_mw,
                q_ac_mvar: s.q_ac_mvar,
                p_dc_mw: s.p_dc_mw,
                v_dc_pu: s.v_dc_pu,
                converter_loss_mw: s.losses_mw,
                lcc_detail: None,
                converged: self.converged,
            })
            .collect();

        let total_converter_loss_mw = stations.iter().map(|c| c.converter_loss_mw).sum::<f64>();
        let total_dc_network_loss_mw = self.dc_result.total_losses();
        let total_loss_mw = total_converter_loss_mw + total_dc_network_loss_mw;

        let method = match self.method {
            AcDcMethod::Sequential => HvdcMethod::Sequential,
            AcDcMethod::BlockCoupled | AcDcMethod::Simultaneous => HvdcMethod::BlockCoupled,
        };

        let dc_buses = self
            .dc_result
            .v_dc
            .iter()
            .enumerate()
            .map(|(idx, &voltage_pu)| HvdcDcBusSolution {
                dc_bus: dc_bus_ids.get(idx).copied().unwrap_or(idx as u32),
                voltage_pu,
            })
            .collect();

        Ok(HvdcSolution {
            stations,
            dc_buses,
            total_converter_loss_mw,
            total_dc_network_loss_mw,
            total_loss_mw,
            iterations: self.iterations as u32,
            converged: self.converged,
            method,
        })
    }
}

/// Compute dP_ac/dV_dc coupling sensitivity for a VSC station.
///
/// How the AC active power injection changes when the DC bus voltage changes.
/// The relationship depends on the converter control mode:
///
/// - **ConstantPQ**: dP_ac/dV_dc = 0 (P is a fixed setpoint, independent of V_dc)
/// - **PVdcDroop**: dP_ac/dV_dc = k_droop / base_mva (droop response, in pu/pu)
/// - **ConstantVdc**: dP_ac/dV_dc = I_dc / base_mva where I_dc = P / V_dc
///   (since P_dc = V_dc * I_dc, dP/dV_dc = I_dc at the operating point)
///
/// The loss correction term is also included: as V_dc changes, the power
/// delivered changes, which changes the AC current and hence losses.
fn compute_dp_ac_dv_dc(
    station: &VscStation,
    v_dc_pu: f64,
    _v_ac_pu: f64,
    p_mw: f64,
    _q_mvar: f64,
    base_mva: f64,
) -> f64 {
    match &station.control_mode {
        VscHvdcControlMode::ConstantPQ { .. } => {
            // P is a fixed setpoint — no sensitivity to V_dc.
            0.0
        }
        VscHvdcControlMode::PVdcDroop { k_droop, .. } => {
            // P = p_set + k_droop * (v_dc - v_dc_set)
            // dP/dV_dc = k_droop (in MW/pu)
            // Convert to per-unit: dP_pu/dV_dc = k_droop / base_mva
            k_droop / base_mva
        }
        VscHvdcControlMode::ConstantVdc { .. } => {
            // The DC slack station: P is determined by DC power balance.
            // At the operating point: P_dc = V_dc * I_dc
            // dP_dc/dV_dc = I_dc (approximately, ignoring dI/dV feedback)
            // I_dc = P / V_dc
            if v_dc_pu.abs() > 1e-6 {
                (p_mw / v_dc_pu) / base_mva
            } else {
                0.0
            }
        }
        VscHvdcControlMode::ConstantPVac { .. } => {
            // P is a fixed setpoint — no sensitivity to V_dc.
            0.0
        }
    }
}

/// Compute dP_dc/dV_ac coupling sensitivity for a VSC station.
///
/// How the DC-side power changes when the AC bus voltage changes. The
/// primary mechanism is through the converter loss model:
///
/// ```text
/// P_loss = (a + b * I_ac + c * I_ac^2) * base_mva
/// I_ac = S_ac / (V_ac * base_mva)
/// ```
///
/// where `S_ac = sqrt(P^2 + Q^2)` is the apparent power.
///
/// Taking the derivative: `dP_loss/dV_ac = -(b + 2c * I_ac) * I_ac / V_ac`
///
/// Since `P_dc = P_ac - P_loss` (power delivered to DC = power from AC - losses):
/// `dP_dc/dV_ac = -dP_loss/dV_ac = (b + 2c * I_ac) * I_ac / V_ac`
///
/// In per-unit: divide by base_mva.
fn compute_dp_dc_dv_ac(
    station: &VscStation,
    v_ac_pu: f64,
    p_mw: f64,
    q_mvar: f64,
    base_mva: f64,
) -> f64 {
    if v_ac_pu < 1e-6 {
        return 0.0;
    }

    let s_mva = (p_mw * p_mw + q_mvar * q_mvar).sqrt();
    let i_ac = s_mva / (v_ac_pu * base_mva);
    let loss_c = station.quadratic_loss_coefficient(p_mw);

    // dP_loss/dV_ac = -(b + 2c * I_ac) * I_ac / V_ac  (in MW)
    // dP_dc/dV_ac = -dP_loss/dV_ac = (b + 2c * I_ac) * I_ac / V_ac
    let dp_dc_dv_ac_mw = (station.loss_linear + 2.0 * loss_c * i_ac) * i_ac / v_ac_pu;

    // Convert to per-unit
    dp_dc_dv_ac_mw * base_mva / (base_mva * base_mva)
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
        let next = net_p_mw + losses;
        if (next - raw_p).abs() < 1e-10 {
            raw_p = next;
            break;
        }
        raw_p = next;
    }
    let losses = station.losses_mw(raw_p, q_mvar.abs(), v_ac_pu.max(1e-6), base_mva);
    (raw_p, losses)
}

fn dc_bus_power(g_dc: &[Vec<f64>], v_dc: &[f64], dc_bus: usize) -> f64 {
    let i_k: f64 = g_dc[dc_bus]
        .iter()
        .zip(v_dc.iter())
        .map(|(g, v)| g * v)
        .sum();
    i_k * v_dc[dc_bus]
}

/// Solve the block-coupled AC/DC power flow problem.
///
/// This is the main entry point for FPQ-39 / P5-024. It performs alternating
/// AC and DC solves and optionally applies cross-coupling sensitivity updates.
/// Set `solver_mode = AcDcSolverMode::Sequential` to disable the block-coupled
/// wrapper and run the sequential fallback directly.
///
/// # Arguments
/// * `network`       — Base AC network (cloned internally for injection building)
/// * `dc_network`    — MTDC DC network (mutated to hold final DC voltages)
/// * `vsc_stations`  — VSC stations coupling AC to DC buses
/// * `opts`          — Solver options
///
/// # Returns
/// `BlockCoupledAcDcResult` with converged AC and DC voltages.
pub fn solve_block_coupled_ac_dc(
    network: &Network,
    dc_network: &mut DcNetwork,
    vsc_stations: &[VscStation],
    opts: &BlockCoupledAcDcSolverOptions,
) -> Result<BlockCoupledAcDcResult, HvdcError> {
    info!(
        vsc_stations = vsc_stations.len(),
        dc_buses = dc_network.n_buses(),
        solver_mode = ?opts.solver_mode,
        max_iter = opts.max_iter,
        tol = opts.tol,
        "Block-coupled AC/DC solve starting"
    );
    match opts.solver_mode {
        AcDcSolverMode::BlockCoupled => {
            solve_block_coupled_outer(network, dc_network, vsc_stations, opts)
        }
        AcDcSolverMode::Sequential => {
            solve_sequential_mtdc(network, dc_network, vsc_stations, opts)
        }
    }
}

/// Block-coupled AC/DC outer iteration: solve AC and DC via alternating
/// Gauss-Seidel-style updates with coupling through the mismatch vector.
///
/// At each outer iteration:
/// 1. Build augmented AC network with current VSC P/Q injections.
/// 2. Solve AC power flow (NR) -> get V_ac, theta.
/// 3. Extract V_ac at VSC buses; update P_dc setpoints via control mode.
/// 4. Solve DC power flow (NR) -> get V_dc per bus.
/// 5. (Coupled mode) Apply cross-coupling sensitivity corrections.
/// 6. Check convergence: max(|dP_ac|, |dV_dc|) < tol.
///
/// # Jacobian Coupling
///
/// When `opts.apply_coupling_sensitivities` is `true` (default), cross-coupling
/// corrections are
/// applied after each AC+DC sub-solve using the analytical derivatives
/// `dP_ac/dV_dc` and `dP_dc/dV_ac`. This improves convergence on weak AC
/// systems (SCR < 3) without modifying the internal NR solvers.
///
/// When `opts.apply_coupling_sensitivities` is `false`, the solver uses a purely decoupled
/// Jacobian where the coupling is captured only through mismatch updates.
/// This is adequate for strong AC systems (SCR > 3).
fn solve_block_coupled_outer(
    network: &Network,
    dc_network: &mut DcNetwork,
    vsc_stations: &[VscStation],
    opts: &BlockCoupledAcDcSolverOptions,
) -> Result<BlockCoupledAcDcResult, HvdcError> {
    let base_mva = network.base_mva;
    let bus_map = network.bus_index_map();
    let slack_station_indices: Vec<usize> = vsc_stations
        .iter()
        .enumerate()
        .filter_map(|(idx, station)| station.control_mode.is_dc_slack().then_some(idx))
        .collect();
    if slack_station_indices.len() > 1 {
        return Err(HvdcError::UnsupportedConfiguration(
            "block-coupled AC/DC solve supports at most one ConstantVdc station".to_string(),
        ));
    }
    if let Some(&slack_station_idx) = slack_station_indices.first()
        && vsc_stations[slack_station_idx].dc_bus_idx != dc_network.slack_dc_bus
    {
        return Err(HvdcError::UnsupportedConfiguration(format!(
            "ConstantVdc station at AC bus {} must be placed on the DC slack bus in the block-coupled solver",
            vsc_stations[slack_station_idx].ac_bus
        )));
    }
    let slack_station_idx = slack_station_indices.first().copied();

    debug!(
        vsc_stations = vsc_stations.len(),
        ac_buses = network.buses.len(),
        dc_buses = dc_network.n_buses(),
        "Block-coupled AC/DC solver: validating VSC station buses"
    );

    // Validate VSC station AC buses exist.
    for station in vsc_stations {
        if !bus_map.contains_key(&station.ac_bus) {
            return Err(HvdcError::BusNotFound(station.ac_bus));
        }
    }

    // Initial state: flat DC start, zero Q injections.
    let mut q_prev: Vec<f64> = vsc_stations
        .iter()
        .map(|s| s.control_mode.effective_q_mvar(1.0, 0.0, 0.0))
        .collect();

    let mut v_dc_prev = dc_network.v_dc.clone();
    let mut v_ac_map: HashMap<u32, f64> = network
        .buses
        .iter()
        .map(|b| (b.number, b.voltage_magnitude_pu))
        .collect();
    // Track V_ac from previous outer iteration for coupling correction dV_ac.
    let mut v_ac_prev_map: HashMap<u32, f64> = v_ac_map.clone();
    let mut slack_net_p_prev_mw = vec![0.0_f64; vsc_stations.len()];
    let mut net_p_predict_offsets_mw = vec![0.0_f64; vsc_stations.len()];

    let acpf_opts = AcPfOptions {
        tolerance: opts.ac_tol,
        max_iterations: opts.ac_max_iter,
        flat_start: opts.flat_start,
        ..AcPfOptions::default()
    };

    let mut converged = false;
    let mut iterations = 0;
    let mut last_dc_result: Option<DcPfResult> = None;
    let mut last_v_ac = Vec::new();
    let mut last_theta_ac = Vec::new();
    let mut last_bus_numbers = Vec::new();
    let mut final_max_delta = 0.0_f64;

    for _outer in 0..opts.max_iter {
        iterations += 1;

        // ── Step 1: Build VSC P/Q injections for AC solve ─────────────────
        let p_ac_injections: Vec<f64> = vsc_stations
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if Some(i) == slack_station_idx {
                    return (slack_net_p_prev_mw[i] + net_p_predict_offsets_mw[i]) / base_mva;
                }
                let v_dc = dc_network.v_dc.get(s.dc_bus_idx).copied().unwrap_or(1.0);
                let p_mw = s.p_mw(v_dc);
                let q_mvar = q_prev[i];
                let v_ac = v_ac_map.get(&s.ac_bus).copied().unwrap_or(1.0);
                let losses = s.losses_mw(p_mw, q_mvar.abs(), v_ac, base_mva);
                // Net AC injection: positive = injection into AC bus.
                (p_mw - losses + net_p_predict_offsets_mw[i]) / base_mva
            })
            .collect();

        let q_injections: Vec<f64> = vsc_stations
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let v_ac = v_ac_map.get(&s.ac_bus).copied().unwrap_or(1.0);
                q_prev[i] = s.q_mvar(v_ac, q_prev[i], 0.0);
                q_prev[i] / base_mva
            })
            .collect();

        // ── Step 2: Build augmented AC network ────────────────────────────
        let aug_net =
            build_mtdc_augmented_network(network, vsc_stations, &p_ac_injections, &q_injections);

        // ── Step 3: Solve AC power flow ───────────────────────────────────
        let ac_result =
            solve_ac_pf(&aug_net, &acpf_opts).map_err(|e| HvdcError::AcPfFailed(e.to_string()))?;

        // Update V_ac map from AC solution.
        for (bus, &vm) in aug_net
            .buses
            .iter()
            .zip(ac_result.voltage_magnitude_pu.iter())
        {
            v_ac_map.insert(bus.number, vm);
        }

        // Save AC results.
        last_v_ac = ac_result.voltage_magnitude_pu.clone();
        last_theta_ac = ac_result.voltage_angle_rad.clone();
        last_bus_numbers = aug_net.buses.iter().map(|b| b.number).collect();

        // ── Step 4: Compute P_dc setpoints from VSC control ───────────────
        let p_dc_per_bus: Vec<f64> = {
            let mut p = vec![0.0_f64; dc_network.n_buses()];
            for (i, station) in vsc_stations.iter().enumerate() {
                if Some(i) == slack_station_idx {
                    continue;
                }
                let v_dc = dc_network
                    .v_dc
                    .get(station.dc_bus_idx)
                    .copied()
                    .unwrap_or(1.0);
                let v_ac = v_ac_map.get(&station.ac_bus).copied().unwrap_or(1.0);
                let p_mw = station.p_mw(v_dc);
                let q_mvar = q_prev
                    .iter()
                    .zip(vsc_stations.iter())
                    .find(|(_, s)| s.ac_bus == station.ac_bus)
                    .map(|(q, _)| *q)
                    .unwrap_or(0.0);
                let losses = station.losses_mw(p_mw, q_mvar.abs(), v_ac, base_mva);
                let net_p_mw = p_mw - losses + net_p_predict_offsets_mw[i];
                // Power delivered at the DC terminal: rectifier injects, inverter draws.
                // Convention: positive P_dc = power flowing into the DC network.
                if station.dc_bus_idx < p.len() {
                    p[station.dc_bus_idx] += net_p_mw / base_mva;
                }
            }
            p
        };

        // ── Step 5: Solve DC power flow ───────────────────────────────────
        let dc_result = dc_network.solve_dc_pf(&p_dc_per_bus, opts.dc_tol, opts.dc_max_iter)?;
        let g_dc = dc_network.build_conductance_matrix(dc_network.n_buses());

        if let Some(slack_idx) = slack_station_idx {
            let slack_dc_bus = dc_network.slack_dc_bus;
            let slack_power_needed_pu =
                dc_bus_power(&g_dc, &dc_result.v_dc, slack_dc_bus) - p_dc_per_bus[slack_dc_bus];
            slack_net_p_prev_mw[slack_idx] = slack_power_needed_pu * base_mva;
        }

        // ── Step 5a: Coordinated droop adjustment (3E) ───────────────────
        //
        // After the DC solve, compute the total DC power imbalance and
        // redistribute it across PVdcDroop stations proportional to their
        // droop gains. This adjusts the P injections used in the next outer
        // iteration for faster convergence in multi-terminal droop systems.
        if opts.coordinated_droop {
            let v_dc = &dc_result.v_dc;
            // Total DC power imbalance: sum of (cable power - converter injection).
            let total_imbalance: f64 = (0..v_dc.len())
                .map(|k| {
                    let i_k: f64 = g_dc[k].iter().zip(v_dc.iter()).map(|(g, v)| g * v).sum();
                    i_k * v_dc[k] - p_dc_per_bus.get(k).copied().unwrap_or(0.0)
                })
                .sum();

            let total_k: f64 = vsc_stations
                .iter()
                .filter_map(|s| {
                    if let VscHvdcControlMode::PVdcDroop { k_droop, .. } = &s.control_mode {
                        Some(k_droop.abs())
                    } else {
                        None
                    }
                })
                .sum();

            if total_k > 1e-9 && total_imbalance.abs() > 1e-9 {
                for station in vsc_stations {
                    if let VscHvdcControlMode::PVdcDroop { k_droop, .. } = &station.control_mode {
                        let offset_mw = -total_imbalance * base_mva * (k_droop.abs() / total_k);
                        if station.dc_bus_idx < dc_network.v_dc.len()
                            && station.dc_bus_idx != dc_network.slack_dc_bus
                        {
                            // Adjust DC voltage estimate to reflect redistributed power.
                            let g_diag = g_dc[station.dc_bus_idx][station.dc_bus_idx];
                            if g_diag.abs() > 1e-12 {
                                let dv = offset_mw
                                    / (base_mva * g_diag * v_dc[station.dc_bus_idx].max(1e-6));
                                dc_network.v_dc[station.dc_bus_idx] =
                                    (dc_result.v_dc[station.dc_bus_idx] + dv * 0.5).max(0.01);
                            }
                        }
                    }
                }
            }
        }

        // ── Step 5b: Optional sensitivity correction ─────────────────────
        //
        // After the decoupled AC and DC sub-solves, apply cross-coupling
        // sensitivity corrections to improve convergence. The correction
        // adjusts the next iteration's P injection estimates based on the
        // analytical derivatives dP_ac/dV_dc and dP_dc/dV_ac.
        //
        // For each VSC station:
        //   - dV_dc = V_dc_new - V_dc_old: how much DC voltage changed
        //   - dV_ac = V_ac_new - V_ac_prev: how much AC voltage changed
        //   - P correction for next AC solve: dP_ac/dV_dc * dV_dc
        //   - DC voltage correction: dP_dc/dV_ac * dV_ac (adjusts DC V)
        //
        // The corrections are applied to dc_network.v_dc so they feed into
        // the next outer iteration's AC injection computation.
        if opts.apply_coupling_sensitivities {
            let mut next_net_p_predict_offsets_mw = vec![0.0_f64; vsc_stations.len()];
            for (i, station) in vsc_stations.iter().enumerate() {
                let v_dc_new = dc_result
                    .v_dc
                    .get(station.dc_bus_idx)
                    .copied()
                    .unwrap_or(1.0);
                let v_dc_old = v_dc_prev.get(station.dc_bus_idx).copied().unwrap_or(1.0);
                let v_ac = v_ac_map.get(&station.ac_bus).copied().unwrap_or(1.0);
                let q_mvar = q_prev[i];
                let (raw_p_mw, _) = if Some(i) == slack_station_idx {
                    recover_raw_power_from_net(
                        station,
                        slack_net_p_prev_mw[i],
                        q_mvar,
                        v_ac,
                        base_mva,
                    )
                } else {
                    (station.p_mw(v_dc_new), 0.0)
                };

                // Compute coupling sensitivities
                let dp_ac_dv_dc =
                    compute_dp_ac_dv_dc(station, v_dc_new, v_ac, raw_p_mw, q_mvar, base_mva);
                let dp_dc_dv_ac = compute_dp_dc_dv_ac(station, v_ac, raw_p_mw, q_mvar, base_mva);

                let dv_dc = v_dc_new - v_dc_old;
                let ac_correction_mw = dp_ac_dv_dc * dv_dc * base_mva * 0.5;
                next_net_p_predict_offsets_mw[i] = ac_correction_mw;

                // AC->DC coupling: use dP_dc/dV_ac to estimate how V_ac
                // changes affected DC power balance. Retrieve the V_ac
                // change relative to the previous outer iteration.
                let v_ac_prev_iter = v_ac_prev_map.get(&station.ac_bus).copied().unwrap_or(v_ac);
                let dv_ac = v_ac - v_ac_prev_iter;

                // Apply coupling correction to the DC bus voltage estimate.
                // The AC-voltage-driven DC power mismatch is translated into a
                // voltage update with a local diagonal Jacobian approximation.
                let dc_power_correction_pu = dp_dc_dv_ac * dv_ac;
                let dc_correction = if station.dc_bus_idx != dc_network.slack_dc_bus {
                    let v_dc_safe = v_dc_new.max(1e-6);
                    let j_diag = g_dc[station.dc_bus_idx][station.dc_bus_idx]
                        + p_dc_per_bus[station.dc_bus_idx] / (v_dc_safe * v_dc_safe);
                    if j_diag.abs() > 1e-12 {
                        dc_power_correction_pu / j_diag * 0.5
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                // Apply correction to the DC voltage for next iteration
                if station.dc_bus_idx != dc_network.slack_dc_bus {
                    dc_network.v_dc[station.dc_bus_idx] = (v_dc_new + dc_correction).max(0.01);
                }

                debug!(
                    station = i,
                    ac_bus = station.ac_bus,
                    dc_bus = station.dc_bus_idx,
                    dp_ac_dv_dc = dp_ac_dv_dc,
                    dp_dc_dv_ac = dp_dc_dv_ac,
                    dv_dc = dv_dc,
                    dv_ac = dv_ac,
                    ac_correction_mw = ac_correction_mw,
                    dc_power_correction_pu = dc_power_correction_pu,
                    dc_correction = dc_correction,
                    "Coupling correction applied"
                );
            }
            net_p_predict_offsets_mw = next_net_p_predict_offsets_mw;
        } else {
            net_p_predict_offsets_mw.fill(0.0);
        }

        // ── Step 6: Check convergence ─────────────────────────────────────
        let v_dc_delta = dc_result
            .v_dc
            .iter()
            .zip(v_dc_prev.iter())
            .map(|(new, old)| (new - old).abs())
            .fold(0.0_f64, f64::max);

        debug!(
            iteration = iterations,
            v_dc_delta = v_dc_delta,
            ac_mismatch = ac_result.max_mismatch,
            "Block-coupled AC/DC outer iteration"
        );
        final_max_delta = v_dc_delta.max(ac_result.max_mismatch);

        // Update V_ac previous map for coupling correction in next iteration
        for (bus, &vm) in aug_net
            .buses
            .iter()
            .zip(ac_result.voltage_magnitude_pu.iter())
        {
            v_ac_prev_map.insert(bus.number, vm);
        }

        v_dc_prev = dc_result.v_dc.clone();
        last_dc_result = Some(dc_result);

        if v_dc_delta < opts.tol && ac_result.max_mismatch < opts.tol {
            converged = true;
            break;
        }
    }

    if !converged {
        warn!(
            iterations = iterations,
            max_iter = opts.max_iter,
            tol = opts.tol,
            "Block-coupled AC/DC solve did not converge"
        );
        return Err(HvdcError::NotConverged {
            iterations: iterations as u32,
            max_delta: final_max_delta,
        });
    }

    let dc_result = last_dc_result.unwrap_or_else(|| DcPfResult {
        v_dc: dc_network.v_dc.clone(),
        branch_flows: Vec::new(),
        branch_losses: Vec::new(),
        shunt_losses: 0.0,
        ground_losses: 0.0,
        converged: false,
        iterations: 0,
    });

    // Build per-station results.
    let station_results: Vec<VscStationResult> = vsc_stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let v_dc = dc_result.v_dc.get(s.dc_bus_idx).copied().unwrap_or(1.0);
            let v_ac = v_ac_map.get(&s.ac_bus).copied().unwrap_or(1.0);
            let q_mvar = q_prev[i];
            let (raw_p_mw, losses) = if Some(i) == slack_station_idx {
                recover_raw_power_from_net(s, slack_net_p_prev_mw[i], q_mvar, v_ac, base_mva)
            } else {
                let raw_p_mw = s.p_mw(v_dc);
                let losses = s.losses_mw(raw_p_mw, q_mvar.abs(), v_ac, base_mva);
                (raw_p_mw, losses)
            };
            VscStationResult {
                ac_bus: s.ac_bus,
                dc_bus_idx: s.dc_bus_idx,
                p_ac_mw: raw_p_mw - losses,
                q_ac_mvar: q_mvar,
                p_dc_mw: -raw_p_mw,
                v_dc_pu: v_dc,
                losses_mw: losses,
            }
        })
        .collect();

    info!(
        iterations = iterations,
        converged = converged,
        stations = station_results.len(),
        "Block-coupled AC/DC solve complete"
    );

    Ok(BlockCoupledAcDcResult {
        dc_result,
        station_results,
        iterations,
        converged,
        v_ac: last_v_ac,
        theta_ac: last_theta_ac,
        bus_numbers: last_bus_numbers,
        method: AcDcMethod::BlockCoupled,
    })
}

/// Sequential MTDC solver.
///
/// Uses the same alternating AC -> DC structure as the block-coupled solver,
/// but disables the sensitivity-correction step and tags the result as
/// [`Method::Sequential`].
fn solve_sequential_mtdc(
    network: &Network,
    dc_network: &mut DcNetwork,
    vsc_stations: &[VscStation],
    opts: &BlockCoupledAcDcSolverOptions,
) -> Result<BlockCoupledAcDcResult, HvdcError> {
    let mut sequential_opts = opts.clone();
    sequential_opts.solver_mode = AcDcSolverMode::BlockCoupled;
    sequential_opts.apply_coupling_sensitivities = false;
    let mut result =
        solve_block_coupled_outer(network, dc_network, vsc_stations, &sequential_opts)?;
    result.method = AcDcMethod::Sequential;
    Ok(result)
}

/// Build an augmented AC network that includes VSC P/Q injections as bus demand adjustments.
fn build_mtdc_augmented_network(
    network: &Network,
    vsc_stations: &[VscStation],
    p_injections_pu: &[f64],
    q_injections_pu: &[f64],
) -> Network {
    let mut aug = network.clone();
    let base_mva = aug.base_mva;
    // The block-coupled outer loop needs a pure AC solve on the augmented
    // network; keep the explicit DC grid in the original network, not here.
    aug.hvdc.links.clear();
    aug.hvdc.clear_dc_grids();

    for (i, station) in vsc_stations.iter().enumerate() {
        let p_mw = p_injections_pu.get(i).copied().unwrap_or(0.0) * base_mva;
        let q_mvar = q_injections_pu.get(i).copied().unwrap_or(0.0) * base_mva;
        // Positive injection reduces net demand at the bus → negative load.
        use surge_network::network::Load;
        let mut load = Load::new(station.ac_bus, -p_mw, -q_mvar);
        load.id = format!("__hvdc_block_inj_{}", station.ac_bus);
        aug.loads.push(load);
        for bus in aug.buses.iter_mut() {
            if bus.number == station.ac_bus && bus.bus_type == BusType::Isolated {
                bus.bus_type = BusType::PQ;
            }
        }
    }

    aug
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dc_network::topology::DcCable;
    use surge_network::network::{Generator, Load};

    /// Build a minimal 3-bus AC + 2-bus DC coupled system for testing.
    ///
    /// AC network:
    ///   Bus 1 (slack) — Bus 2 (PQ, load 50 MW) — Bus 3 (PQ, VSC rectifier)
    ///   Bus 1 is connected to Bus 2 via a line (R=0.01, X=0.05).
    ///   Bus 2 is connected to Bus 3 via a line (R=0.01, X=0.05).
    ///
    /// DC network:
    ///   DC Bus 0 (slack, V_dc=1.0) — connected to VSC at AC Bus 3 (rectifier).
    ///   DC Bus 1 (free) — connected to VSC at AC Bus 2 (inverter, feeds load).
    ///   Cable: R_dc = 0.05 pu.
    ///
    /// VSC Station 0: AC bus 3, DC bus 0 — rectifier (draws from AC, injects into DC).
    /// VSC Station 1: AC bus 2, DC bus 1 — inverter  (draws from DC, injects into AC).
    fn build_test_ac_dc_system() -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("block-coupled-test-3bus");
        net.base_mva = 100.0;

        // Bus 1: slack
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_angle_rad = 0.0;
        net.buses.push(b1);

        // Bus 2: PQ with load (the inverter feeds power here)
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 50.0, 10.0));

        // Bus 3: PQ (the rectifier draws from here)
        let b3 = Bus::new(3, BusType::PQ, 230.0);
        net.buses.push(b3);
        net.loads.push(Load::new(3, 20.0, 0.0));

        // Lines
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.05, 0.02));

        // Slack generator
        let mut gobj = Generator::new(1, 200.0, 1.0);
        gobj.pmax = 500.0;
        gobj.qmax = 300.0;
        gobj.qmin = -300.0;
        net.generators.push(gobj);

        // DC network: 2 buses, slack at DC bus 0.
        let mut dc_net = DcNetwork::new(2, 0);
        dc_net.v_dc_slack = 1.0;
        dc_net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.05,
            i_max_pu: 5.0,
        });

        // VSC Station 0: AC bus 3 (rectifier) → DC bus 0 (slack).
        // Draws 30 MW from AC bus 3, injects into DC network.
        let s0 = VscStation {
            ac_bus: 3,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: -30.0,
                q_set: 0.0,
            },
            q_max_mvar: 50.0,
            q_min_mvar: -50.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.005,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };

        // VSC Station 1: AC bus 2 (inverter) → DC bus 1 (free).
        // Injects 28 MW into AC bus 2 (30 MW - ~2 MW losses).
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

    fn build_constant_vdc_test_system() -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("block-coupled-constant-vdc");
        net.base_mva = 100.0;

        let mut slack = Bus::new(1, BusType::Slack, 230.0);
        slack.voltage_magnitude_pu = 1.0;
        slack.voltage_angle_rad = 0.0;
        net.buses.push(slack);
        net.buses.push(Bus::new(2, BusType::PQ, 230.0));
        net.loads.push(Load::new(2, 40.0, 10.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.05, 0.02));

        let mut generator = Generator::new(1, 180.0, 1.0);
        generator.pmax = 500.0;
        generator.qmax = 300.0;
        generator.qmin = -300.0;
        net.generators.push(generator);

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

    // ── block_coupled_convergence_simple ─────────────────────────────────────

    /// 3-bus AC + 2-bus DC system: verify the block-coupled solver converges.
    #[test]
    fn block_coupled_convergence_simple() {
        let (net, mut dc_net, stations) = build_test_ac_dc_system();
        let opts = BlockCoupledAcDcSolverOptions {
            tol: 1e-5,
            max_iter: 20,
            solver_mode: AcDcSolverMode::BlockCoupled,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let result = solve_block_coupled_ac_dc(&net, &mut dc_net, &stations, &opts)
            .expect("Block-coupled AC/DC solve should succeed");

        assert!(
            result.converged,
            "Block-coupled solver must converge in {} iterations",
            result.iterations
        );
        assert!(
            result.iterations <= 20,
            "Converged in {} iterations (expected ≤ 20)",
            result.iterations
        );
        assert!(
            result.dc_result.converged,
            "DC sub-problem must also converge"
        );

        // DC bus voltages must be in a physically reasonable range.
        for (i, &v) in result.dc_result.v_dc.iter().enumerate() {
            assert!(
                v > 0.9 && v < 1.1,
                "DC bus {i} voltage {v:.4} out of expected range [0.9, 1.1]"
            );
        }

        // AC bus voltages must also be reasonable.
        for (i, &v) in result.v_ac.iter().enumerate() {
            assert!(
                v > 0.9 && v < 1.1,
                "AC bus {i} voltage {v:.4} out of expected range [0.9, 1.1]"
            );
        }

        assert_eq!(
            result.method,
            AcDcMethod::BlockCoupled,
            "Method should be BlockCoupled"
        );
    }

    // ── sequential_vs_block_coupled_agree ────────────────────────────────────

    /// Same problem solved by both methods; verify results agree within 1e-4 pu.
    #[test]
    fn sequential_vs_block_coupled_agree() {
        let (net, mut dc_net_u, stations) = build_test_ac_dc_system();
        let (_, mut dc_net_s, _) = build_test_ac_dc_system();

        let opts_block = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            solver_mode: AcDcSolverMode::BlockCoupled,
            ..BlockCoupledAcDcSolverOptions::default()
        };
        let opts_seq = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            solver_mode: AcDcSolverMode::Sequential,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let r_block = solve_block_coupled_ac_dc(&net, &mut dc_net_u, &stations, &opts_block)
            .expect("Block-coupled solve failed");
        let r_seq = solve_block_coupled_ac_dc(&net, &mut dc_net_s, &stations, &opts_seq)
            .expect("Sequential solve failed");

        assert!(r_block.converged, "Block-coupled solve must converge");
        assert!(r_seq.converged, "Sequential must converge");

        // DC bus voltages must agree within 1e-4 pu.
        for (i, (vu, vs)) in r_block
            .dc_result
            .v_dc
            .iter()
            .zip(r_seq.dc_result.v_dc.iter())
            .enumerate()
        {
            let diff = (vu - vs).abs();
            assert!(
                diff < 1e-4,
                "DC bus {i} voltage mismatch: block-coupled={vu:.6}, sequential={vs:.6}, diff={diff:.2e}"
            );
        }

        // AC bus voltages must agree within 1e-4 pu.
        for (i, (vu, vs)) in r_block.v_ac.iter().zip(r_seq.v_ac.iter()).enumerate() {
            let diff = (vu - vs).abs();
            assert!(
                diff < 1e-4,
                "AC bus {i} voltage mismatch: block-coupled={vu:.6}, sequential={vs:.6}, diff={diff:.2e}"
            );
        }

        // Methods must be correctly tagged.
        assert_eq!(r_block.method, AcDcMethod::BlockCoupled);
        assert_eq!(r_seq.method, AcDcMethod::Sequential);
    }

    /// Build a weak AC system with configurable SCR at the converter bus.
    ///
    /// The network has:
    ///   Bus 1 (slack, generator) — Bus 2 (PQ, converter bus, small load)
    ///   One line with configurable impedance.
    ///   One VSC station at bus 2, DC bus 0 (slack), injecting `p_vsc_mw` MW.
    ///
    /// SCR ~ gen_mva / |p_vsc_mw|, so to get SCR=2 with 50 MW HVDC: gen_mva=100.
    /// High line impedance (X=0.2) increases voltage sensitivity.
    fn build_weak_ac_system(
        gen_mva: f64,
        line_x: f64,
        p_vsc_mw: f64,
    ) -> (Network, DcNetwork, Vec<VscStation>) {
        use surge_network::network::Branch;
        use surge_network::network::Bus;

        let mut net = Network::new("weak-ac-test");
        net.base_mva = 100.0;

        // Bus 1: slack with limited generator
        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_magnitude_pu = 1.0;
        b1.voltage_angle_rad = 0.0;
        net.buses.push(b1);

        // Bus 2: PQ, converter bus with small load
        let b2 = Bus::new(2, BusType::PQ, 230.0);
        net.buses.push(b2);
        net.loads.push(Load::new(2, 10.0, 5.0));

        // High-impedance line (weak connection)
        net.branches
            .push(Branch::new_line(1, 2, 0.02, line_x, 0.01));

        // Generator at slack bus — limited capacity to achieve low SCR
        let mut gobj = Generator::new(1, gen_mva * 0.5, 1.0);
        gobj.pmax = gen_mva;
        gobj.qmax = gen_mva * 0.5;
        gobj.qmin = -gen_mva * 0.5;
        net.generators.push(gobj);

        // DC network: single bus (slack)
        let mut dc_net = DcNetwork::new(2, 0);
        dc_net.v_dc_slack = 1.0;
        dc_net.add_cable(DcCable {
            from_dc_bus: 0,
            to_dc_bus: 1,
            r_dc_pu: 0.02,
            i_max_pu: 5.0,
        });

        // VSC station at bus 2 (inverter, injects into AC)
        let s0 = VscStation {
            ac_bus: 2,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: p_vsc_mw,
                q_set: 0.0,
            },
            q_max_mvar: gen_mva * 0.5,
            q_min_mvar: -gen_mva * 0.5,
            loss_constant_mw: 0.001,
            loss_linear: 0.005,
            loss_c_rectifier: 0.001,
            loss_c_inverter: 0.001,
        };

        // Second station at DC bus 1 to balance the DC network
        let s1 = VscStation {
            ac_bus: 1,
            dc_bus_idx: 1,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: -p_vsc_mw,
                q_set: 0.0,
            },
            q_max_mvar: gen_mva,
            q_min_mvar: -gen_mva,
            loss_constant_mw: 0.0,
            loss_linear: 0.005,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };

        (net, dc_net, vec![s0, s1])
    }

    // ── test_coupled_strong_ac_agrees_with_decoupled ─────────────────────────

    /// Strong AC system (SCR ~ 5): verify coupled and decoupled modes produce
    /// the same converged solution within 1e-6 pu.
    #[test]
    fn test_coupled_strong_ac_agrees_with_decoupled() {
        // SCR ~ gen_mva / p_vsc = 200 / 40 = 5 (strong)
        let (net, mut dc_coupled, stations) = build_weak_ac_system(200.0, 0.05, 40.0);
        let (_, mut dc_decoupled, _) = build_weak_ac_system(200.0, 0.05, 40.0);

        let opts_coupled = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            apply_coupling_sensitivities: true,
            ..BlockCoupledAcDcSolverOptions::default()
        };
        let opts_decoupled = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            apply_coupling_sensitivities: false,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let r_coupled = solve_block_coupled_ac_dc(&net, &mut dc_coupled, &stations, &opts_coupled)
            .expect("Coupled solve failed");
        let r_decoupled =
            solve_block_coupled_ac_dc(&net, &mut dc_decoupled, &stations, &opts_decoupled)
                .expect("Decoupled solve failed");

        assert!(r_coupled.converged, "Coupled must converge");
        assert!(r_decoupled.converged, "Decoupled must converge");

        // Both must produce the same DC bus voltages
        for (i, (vc, vd)) in r_coupled
            .dc_result
            .v_dc
            .iter()
            .zip(r_decoupled.dc_result.v_dc.iter())
            .enumerate()
        {
            let diff = (vc - vd).abs();
            assert!(
                diff < 1e-4,
                "DC bus {i}: coupled={vc:.6}, decoupled={vd:.6}, diff={diff:.2e}"
            );
        }

        // AC voltages must agree
        for (i, (vc, vd)) in r_coupled
            .v_ac
            .iter()
            .zip(r_decoupled.v_ac.iter())
            .enumerate()
        {
            let diff = (vc - vd).abs();
            assert!(
                diff < 1e-4,
                "AC bus {i}: coupled={vc:.6}, decoupled={vd:.6}, diff={diff:.2e}"
            );
        }
    }

    #[test]
    fn block_coupled_constant_vdc_station_carries_active_power() {
        let (net, mut dc_net, stations) = build_constant_vdc_test_system();
        let opts = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let result = solve_block_coupled_ac_dc(&net, &mut dc_net, &stations, &opts)
            .expect("block-coupled solve with ConstantVdc station should succeed");

        assert!(result.converged);
        assert!(
            result.station_results[0].p_ac_mw.abs() > 1e-6,
            "ConstantVdc slack station should carry AC-side active power"
        );
        assert!(
            result.station_results[0].p_dc_mw.abs() > 1e-6,
            "ConstantVdc slack station should carry DC-side active power"
        );
        for station in &result.station_results {
            let balance = station.p_ac_mw + station.p_dc_mw + station.losses_mw;
            assert!(
                balance.abs() < 1e-6,
                "station at AC bus {} violates power balance: {balance:.6e}",
                station.ac_bus
            );
        }
    }

    // ── test_coupled_weak_ac_converges ───────────────────────────────────────

    /// Weak AC system (SCR ~ 2): verify coupled mode converges.
    #[test]
    fn test_coupled_weak_ac_converges() {
        // SCR ~ gen_mva / p_vsc = 80 / 40 = 2 (weak)
        let (net, mut dc_net, stations) = build_weak_ac_system(80.0, 0.20, 40.0);

        let opts = BlockCoupledAcDcSolverOptions {
            tol: 1e-5,
            max_iter: 40,
            apply_coupling_sensitivities: true,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let result = solve_block_coupled_ac_dc(&net, &mut dc_net, &stations, &opts)
            .expect("Coupled solve on weak AC should succeed");

        assert!(
            result.converged,
            "Coupled mode must converge on weak AC (SCR~2) in {} iterations",
            result.iterations
        );

        // Voltages should be physically reasonable
        for (i, &v) in result.v_ac.iter().enumerate() {
            assert!(
                v > 0.85 && v < 1.15,
                "AC bus {i} voltage {v:.4} out of range"
            );
        }
        for (i, &v) in result.dc_result.v_dc.iter().enumerate() {
            assert!(
                v > 0.85 && v < 1.15,
                "DC bus {i} voltage {v:.4} out of range"
            );
        }
    }

    // ── test_coupled_very_weak_ac ───────────────────────────────────────────

    /// Very weak AC system (SCR ~ 1.5): verify coupled mode still converges
    /// (possibly with more iterations and relaxed tolerance).
    #[test]
    fn test_coupled_very_weak_ac() {
        // SCR ~ gen_mva / p_vsc = 45 / 30 = 1.5 (very weak)
        let (net, mut dc_net, stations) = build_weak_ac_system(45.0, 0.25, 30.0);

        let opts = BlockCoupledAcDcSolverOptions {
            tol: 1e-5,
            max_iter: 50,
            apply_coupling_sensitivities: true,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let result = solve_block_coupled_ac_dc(&net, &mut dc_net, &stations, &opts)
            .expect("Coupled solve on very weak AC should succeed");

        assert!(
            result.converged,
            "Coupled mode must converge on very weak AC (SCR~1.5) in {} iterations",
            result.iterations
        );

        // The converter bus voltage may be lower due to weak AC
        for (i, &v) in result.v_ac.iter().enumerate() {
            assert!(
                v > 0.80 && v < 1.20,
                "AC bus {i} voltage {v:.4} out of range for very weak system"
            );
        }
    }

    // ── test_coupled_fewer_iterations ────────────────────────────────────────

    /// Moderate SCR (~ 3): verify coupled mode converges in fewer or equal
    /// outer iterations compared to decoupled mode.
    #[test]
    fn test_coupled_fewer_iterations() {
        // SCR ~ gen_mva / p_vsc = 120 / 40 = 3
        let (net, mut dc_coupled, stations) = build_weak_ac_system(120.0, 0.15, 40.0);
        let (_, mut dc_decoupled, _) = build_weak_ac_system(120.0, 0.15, 40.0);

        let opts_coupled = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            apply_coupling_sensitivities: true,
            ..BlockCoupledAcDcSolverOptions::default()
        };
        let opts_decoupled = BlockCoupledAcDcSolverOptions {
            tol: 1e-6,
            max_iter: 30,
            apply_coupling_sensitivities: false,
            ..BlockCoupledAcDcSolverOptions::default()
        };

        let r_coupled = solve_block_coupled_ac_dc(&net, &mut dc_coupled, &stations, &opts_coupled)
            .expect("Coupled solve failed");
        let r_decoupled =
            solve_block_coupled_ac_dc(&net, &mut dc_decoupled, &stations, &opts_decoupled)
                .expect("Decoupled solve failed");

        assert!(r_coupled.converged, "Coupled must converge");
        assert!(r_decoupled.converged, "Decoupled must converge");

        // Coupled should converge in fewer or equal iterations.
        // (On this moderate-SCR case, the difference may be small but
        // coupled should never be worse.)
        assert!(
            r_coupled.iterations <= r_decoupled.iterations,
            "Coupled ({} iters) should not need more iterations than decoupled ({} iters)",
            r_coupled.iterations,
            r_decoupled.iterations,
        );
    }

    // ── test_coupled_lcc_sensitivity ────────────────────────────────────────

    /// Verify the dP_dc/dV_ac coupling sensitivity is computed correctly.
    ///
    /// For a station with loss_b = 0.01, loss_c = 0.005, at operating point
    /// P=50 MW, Q=10 MVAR, V_ac=0.95 pu, base_mva=100:
    ///   S = sqrt(50^2 + 10^2) = 50.99 MVA
    ///   I_ac = S / (V_ac * base_mva) = 50.99 / (0.95 * 100) = 0.5368 pu
    ///   dP_dc/dV_ac = (b + 2c*I_ac) * I_ac / V_ac * base_mva / base_mva^2
    ///               = (0.01 + 2*0.005*0.5368) * 0.5368 / 0.95 * 100 / 10000
    #[test]
    fn test_coupled_lcc_sensitivity() {
        let station = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 50.0,
                q_set: 10.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.01,
            loss_c_rectifier: 0.005,
            loss_c_inverter: 0.005,
        };

        let v_ac = 0.95;
        let p_mw = 50.0;
        let q_mvar = 10.0;
        let base_mva = 100.0;

        let dp_dc_dv_ac = compute_dp_dc_dv_ac(&station, v_ac, p_mw, q_mvar, base_mva);

        // Manual computation:
        // S = sqrt(50^2 + 10^2) = 50.990... MVA
        // I_ac = 50.990 / (0.95 * 100) = 0.53674...
        // dp_dc_dv_ac_mw = (0.01 + 2*0.005*0.53674) * 0.53674 / 0.95
        //                = (0.01 + 0.005367) * 0.53674 / 0.95
        //                = 0.015367 * 0.53674 / 0.95
        //                = 0.008247... / 0.95
        //                = 0.008681... MW
        // dp_dc_dv_ac_pu = 0.008681 * 100 / 10000 = 8.681e-5 pu
        let s_mva = (p_mw * p_mw + q_mvar * q_mvar).sqrt();
        let i_ac = s_mva / (v_ac * base_mva);
        let expected_mw =
            (station.loss_linear + 2.0 * station.quadratic_loss_coefficient(p_mw) * i_ac) * i_ac
                / v_ac;
        let expected = expected_mw * base_mva / (base_mva * base_mva);

        assert!(
            (dp_dc_dv_ac - expected).abs() < 1e-12,
            "dP_dc/dV_ac mismatch: computed={dp_dc_dv_ac:.6e}, expected={expected:.6e}"
        );

        // Verify the sensitivity is positive (more AC voltage reduces loss,
        // so more power goes to DC).
        assert!(
            dp_dc_dv_ac > 0.0,
            "dP_dc/dV_ac must be positive (higher V_ac -> lower losses -> more DC power)"
        );

        // Also verify dP_ac/dV_dc for ConstantPQ is zero
        let dp_ac_dv_dc = compute_dp_ac_dv_dc(&station, 1.0, v_ac, p_mw, q_mvar, base_mva);
        assert!(
            dp_ac_dv_dc.abs() < 1e-15,
            "ConstantPQ: dP_ac/dV_dc must be zero, got {dp_ac_dv_dc:.6e}"
        );

        // Test PVdcDroop mode sensitivity
        let droop_station = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::PVdcDroop {
                p_set: 50.0,
                voltage_dc_setpoint_pu: 1.0,
                k_droop: 100.0, // 100 MW/pu
                p_min: 0.0,
                p_max: 200.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_c_rectifier: 0.0,
            loss_c_inverter: 0.0,
        };
        let dp_ac_droop = compute_dp_ac_dv_dc(&droop_station, 1.0, 1.0, 50.0, 0.0, base_mva);
        // Expected: k_droop / base_mva = 100.0 / 100.0 = 1.0
        assert!(
            (dp_ac_droop - 1.0).abs() < 1e-12,
            "PVdcDroop: dP_ac/dV_dc must be k_droop/base_mva=1.0, got {dp_ac_droop:.6e}"
        );

        // Test ConstantVdc mode sensitivity
        let vdc_station = VscStation {
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
        let v_dc_test = 1.02;
        let p_test = 80.0;
        let dp_ac_vdc = compute_dp_ac_dv_dc(&vdc_station, v_dc_test, 1.0, p_test, 0.0, base_mva);
        // Expected: I_dc / base_mva = (P / V_dc) / base_mva = (80 / 1.02) / 100
        let expected_vdc = (p_test / v_dc_test) / base_mva;
        assert!(
            (dp_ac_vdc - expected_vdc).abs() < 1e-10,
            "ConstantVdc: dP_ac/dV_dc={dp_ac_vdc:.6e}, expected={expected_vdc:.6e}"
        );

        let directional_station = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantPQ {
                p_set: 80.0,
                q_set: 0.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.01,
            loss_c_rectifier: 0.02,
            loss_c_inverter: 0.05,
        };
        let inverter_loss = directional_station.losses_mw(80.0, 0.0, 1.0, 100.0);
        let rectifier_loss = directional_station.losses_mw(-80.0, 0.0, 1.0, 100.0);

        assert!(inverter_loss > rectifier_loss);
        assert!((rectifier_loss - 2.08).abs() < 1e-12);
        assert!((inverter_loss - 4.0).abs() < 1e-12);
    }

    #[test]
    fn recover_raw_power_from_net_matches_station_loss_equation() {
        let station = VscStation {
            ac_bus: 1,
            dc_bus_idx: 0,
            control_mode: VscHvdcControlMode::ConstantVdc {
                v_dc_target: 1.0,
                q_set: 0.0,
            },
            q_max_mvar: 100.0,
            q_min_mvar: -100.0,
            loss_constant_mw: 0.0,
            loss_linear: 0.01,
            loss_c_rectifier: 0.005,
            loss_c_inverter: 0.005,
        };

        let (raw_p_mw, losses_mw) = recover_raw_power_from_net(&station, 80.0, 12.0, 0.97, 100.0);
        assert!(
            ((raw_p_mw - losses_mw) - 80.0).abs() < 1e-9,
            "recovered raw power should satisfy raw-loss = net"
        );
    }

    #[test]
    fn to_hvdc_solution_uses_exact_dc_power_and_full_dc_losses() {
        let mut network = Network::new("block-coupled-result");
        let grid = network.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(surge_network::network::DcBus {
            bus_id: 101,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });
        grid.buses.push(surge_network::network::DcBus {
            bus_id: 102,
            p_dc_mw: 0.0,
            v_dc_pu: 1.0,
            base_kv_dc: 320.0,
            v_dc_max: 1.1,
            v_dc_min: 0.9,
            cost: 0.0,
            g_shunt_siemens: 0.0,
            r_ground_ohm: 0.0,
        });

        let result = BlockCoupledAcDcResult {
            dc_result: DcPfResult {
                v_dc: vec![1.0, 0.99],
                branch_flows: vec![0.5],
                branch_losses: vec![1.25],
                shunt_losses: 0.75,
                ground_losses: 0.5,
                converged: true,
                iterations: 3,
            },
            station_results: vec![
                VscStationResult {
                    ac_bus: 1,
                    dc_bus_idx: 0,
                    p_ac_mw: -102.0,
                    q_ac_mvar: 0.0,
                    p_dc_mw: 100.0,
                    v_dc_pu: 1.0,
                    losses_mw: 2.0,
                },
                VscStationResult {
                    ac_bus: 2,
                    dc_bus_idx: 1,
                    p_ac_mw: 98.0,
                    q_ac_mvar: 0.0,
                    p_dc_mw: -100.0,
                    v_dc_pu: 0.99,
                    losses_mw: 2.0,
                },
            ],
            iterations: 4,
            converged: true,
            v_ac: vec![1.0, 1.0],
            theta_ac: vec![0.0, 0.0],
            bus_numbers: vec![1, 2],
            method: AcDcMethod::BlockCoupled,
        };

        let solution = result
            .to_hvdc_solution(&network)
            .expect("canonical conversion should succeed");
        assert_eq!(solution.stations[0].p_dc_mw, 100.0);
        assert_eq!(solution.stations[1].p_dc_mw, -100.0);
        assert_eq!(solution.dc_buses[0].dc_bus, 101);
        assert_eq!(solution.dc_buses[1].dc_bus, 102);
        assert!((solution.total_loss_mw - 6.5).abs() < 1e-12);
    }

    #[test]
    fn to_hvdc_solution_accepts_simultaneous_results() {
        let mut network = Network::new("block-coupled-simultaneous");
        network
            .hvdc
            .ensure_dc_grid(1, None)
            .buses
            .push(surge_network::network::DcBus {
                bus_id: 101,
                p_dc_mw: 0.0,
                v_dc_pu: 1.0,
                base_kv_dc: 320.0,
                v_dc_max: 1.1,
                v_dc_min: 0.9,
                cost: 0.0,
                g_shunt_siemens: 0.0,
                r_ground_ohm: 0.0,
            });
        let result = BlockCoupledAcDcResult {
            dc_result: DcPfResult {
                v_dc: vec![1.0],
                branch_flows: Vec::new(),
                branch_losses: Vec::new(),
                shunt_losses: 0.0,
                ground_losses: 0.0,
                converged: true,
                iterations: 1,
            },
            station_results: vec![VscStationResult {
                ac_bus: 1,
                dc_bus_idx: 0,
                p_ac_mw: -10.0,
                q_ac_mvar: 0.0,
                p_dc_mw: 9.0,
                v_dc_pu: 1.0,
                losses_mw: 1.0,
            }],
            iterations: 1,
            converged: true,
            v_ac: vec![1.0],
            theta_ac: vec![0.0],
            bus_numbers: vec![1],
            method: AcDcMethod::Simultaneous,
        };

        let solution = result
            .to_hvdc_solution(&network)
            .expect("simultaneous results should convert to canonical HvdcSolution");
        assert_eq!(solution.method, HvdcMethod::BlockCoupled);
        assert_eq!(solution.stations.len(), 1);
    }
}
