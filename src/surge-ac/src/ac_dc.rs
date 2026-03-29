// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC/DC sequential power flow solver.
//!
//! Wraps the inner Newton-Raphson (KLU) solver to correctly handle point-to-point
//! HVDC links and explicit DC-grid VSC converters present in `network.hvdc`.
//!
//! # DC modelling modes
//!
//! **`FixedSchedule`** (default): DC power is taken directly from the PSS/E
//! schedule fields (`SETVL`, `VSCHD`). The rectifier bus sees an extra load
//! (P_dc + losses) and the inverter bus sees an extra generator (P_dc).
//! Reactive absorption is estimated from the scheduled operating point.
//! A single NR solve is performed — no iteration with the DC circuit.
//!
//! **`SequentialAcDc`**: Full AC/DC outer-loop iteration:
//! 1. Inject current DC P/Q estimates.
//! 2. Solve AC network.
//! 3. Recompute DC operating point from updated bus voltages.
//! 4. Repeat until DC power change < `dc_tol_mw` or `dc_max_iter` reached.
//!
//! LCC: firing/extinction angles are recomputed each outer iteration from
//! updated AC bus voltages, so reactive absorption converges to the actual
//! operating point.
//!
//! VSC: active power is constant across iterations (losses are affine in P,
//! not voltage-dependent). The outer loop adds value for `AcVoltage`
//! regulation, where the converter bus is treated as a PV bus
//! (voltage-regulated) with Q limits from `VscConverterTerminal.q_min_mvar`/
//! `q_max`. `ReactivePower` mode uses the fixed `ac_setpoint` as a constant Q
//! injection. `VdcControl` mode is not supported by AC power flow yet and is
//! rejected explicitly.
//!
//! When the network contains no DC lines, `solve_ac_pf_with_dc_lines` calls `solve_ac_pf_kernel`
//! directly — zero overhead.

use std::borrow::Cow;
use std::collections::HashMap;
use std::f64::consts::PI;

use surge_network::Network;
use surge_network::network::{BusType, Generator};
use surge_network::network::{LccHvdcControlMode, LccHvdcLink};
use surge_network::network::{VscConverterAcControlMode, VscHvdcControlMode, VscHvdcLink};
use surge_solution::PfSolution;
use tracing::{debug, info, warn};

use crate::control::facts_expansion::expand_facts;
use crate::solver::newton_raphson::{AcPfError, AcPfOptions, DcLineModel, solve_ac_pf_kernel};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Solve AC power flow for a network that may contain DC lines and FACTS devices.
///
/// This is the **universal AC power flow solver** — prefer this over
/// [`solve_ac_pf_kernel`] for all production call sites.  [`solve_ac_pf`] routes
/// through this function, so callers that already use `solve_ac_pf` get DC/FACTS
/// handling automatically.
///
/// Steps performed:
///
/// 1. **FACTS expansion**: SVC, STATCOM, and series-compensation devices are
///    converted to equivalent Branch/Generator modifications before solving.
/// 2. **DC injection**: converter P/Q is applied to the AC network according
///    to `options.dc_line_model`:
///    - `FixedSchedule` (default): reads PSS/E scheduled values, injects
///      once, runs a single NR solve — zero iteration overhead.
///    - `SequentialAcDc`: iterates AC solve ↔ DC operating-point update
///      until `|ΔP| < options.dc_tol_mw` or `options.dc_max_iter` reached.
/// 3. **AC solve**: runs [`solve_ac_pf_kernel`] on the expanded/modified network.
///
/// **Fast path**: when the network has no DC lines and no FACTS devices,
/// this function calls [`solve_ac_pf_kernel`] directly with zero extra overhead.
///
/// # When to call `solve_ac_pf_kernel` directly
///
/// Only when you have pre-processed the network yourself (e.g., `surge-hvdc`
/// builds its own augmented AC-only network before solving) or in unit tests
/// targeting the raw KLU inner loop.
pub(crate) fn solve_ac_pf_with_dc_lines(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    let network = preprocess_ac_pf_network(network)?;
    let has_dc = has_ac_dc_coupling(&network);

    if !has_dc {
        // Fast path: delegate directly to NR.
        return solve_ac_pf_kernel(&network, options);
    }

    match &options.dc_line_model {
        DcLineModel::FixedSchedule => {
            let modified = inject_fixed_schedule_dc(&network);
            solve_ac_pf_kernel(&modified, options)
        }

        DcLineModel::SequentialAcDc => solve_sequential_ac_dc(&network, options),
    }
}

pub(crate) fn preprocess_ac_pf_network(network: &Network) -> Result<Cow<'_, Network>, AcPfError> {
    let network = expand_facts(network);
    reject_unsupported_vsc_modes(&network)?;
    Ok(network)
}

pub(crate) fn prepare_fixed_pattern_ac_network<'a>(
    network: &'a Network,
    options: &AcPfOptions,
) -> Result<Cow<'a, Network>, AcPfError> {
    let network = preprocess_ac_pf_network(network)?;
    if !has_ac_dc_coupling(&network) {
        return Ok(network);
    }

    match options.dc_line_model {
        DcLineModel::FixedSchedule => Ok(Cow::Owned(inject_fixed_schedule_dc(&network))),
        DcLineModel::SequentialAcDc => Err(AcPfError::InvalidOptions(
            "PreparedAcPf does not support dc_line_model=SequentialAcDc because the AC network changes across outer AC/DC iterations".to_string(),
        )),
    }
}

fn has_ac_dc_coupling(network: &Network) -> bool {
    network.hvdc.has_point_to_point_links()
        || network.hvdc.dc_grids.iter().any(|grid| {
            grid.converters
                .iter()
                .any(|converter| converter.as_vsc().is_some())
        })
}

fn reject_unsupported_vsc_modes(network: &Network) -> Result<(), AcPfError> {
    if let Some(link) = network
        .hvdc
        .links
        .iter()
        .filter_map(|link| link.as_vsc())
        .find(|link| link.mode == VscHvdcControlMode::VdcControl)
    {
        return Err(AcPfError::InvalidOptions(format!(
            "VSC-HVDC link '{}' uses VdcControl, which is not supported by AC power flow",
            link.name
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fixed-schedule DC injection
// ---------------------------------------------------------------------------

/// Build a modified network with DC line power pre-injected as constant P/Q.
///
/// For each in-service LCC DC line:
/// - Rectifier bus: `pd += P_rect` and `qd += Q_rect` (absorbs P and lagging Q).
/// - Inverter bus: `pd -= P_inv` (injects P; small Q also absorbed at inverter).
///
/// For each in-service VSC DC line:
/// - Sending converter: `pd += P_send + losses`.
/// - Receiving converter: `pd -= P_recv`, even if converter losses make the
///   receiving-end net power negative.
fn inject_fixed_schedule_dc(network: &Network) -> Network {
    let mut net = network.clone();
    let bus_map: HashMap<u32, usize> = net
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // LCC two-terminal DC lines
    for dc in network.hvdc.links.iter().filter_map(|link| link.as_lcc()) {
        if dc.mode == LccHvdcControlMode::Blocked {
            continue;
        }
        if !dc.rectifier.in_service || !dc.inverter.in_service {
            continue;
        }

        let (p_rect, q_rect, p_inv, q_inv) = compute_lcc_fixed_schedule(dc);

        // Model HVDC injections as synthetic Load objects.
        // Rectifier consumes (positive demand), inverter injects (negative demand).
        use surge_network::network::Load;
        if bus_map.contains_key(&dc.rectifier.bus) {
            let mut load = Load::new(dc.rectifier.bus, p_rect, q_rect);
            load.id = format!("__hvdc_lcc_rect_{}", dc.name);
            net.loads.push(load);
        }
        if bus_map.contains_key(&dc.inverter.bus) {
            let mut load = Load::new(dc.inverter.bus, -p_inv, q_inv);
            load.id = format!("__hvdc_lcc_inv_{}", dc.name);
            net.loads.push(load);
        }

        debug!(
            name = dc.name,
            p_rect, q_rect, p_inv, q_inv, "fixed-schedule LCC DC injection applied"
        );
    }

    // VSC DC lines
    for vsc in network.hvdc.links.iter().filter_map(|link| link.as_vsc()) {
        apply_vsc_fixed_schedule(&mut net, vsc, &bus_map);
    }

    // Explicit DC-grid VSC converters use the canonical `DcGrid` model directly.
    apply_explicit_vsc_converters(&mut net, &network.hvdc.dc_grids, &bus_map);

    net
}

/// Compute the fixed-schedule power for one LCC two-terminal DC line.
///
/// Returns `(P_rect_MW, Q_rect_MVAr, P_inv_MW, Q_inv_MVAr)`.
///
/// Uses the scheduled DC current and voltage to estimate converter
/// reactive power absorption via the cosine of the equivalent firing angle.
fn compute_lcc_fixed_schedule(dc: &LccHvdcLink) -> (f64, f64, f64, f64) {
    let (i_dc_ka, vd_r_kv, vd_i_kv) = match dc.mode {
        LccHvdcControlMode::PowerControl => {
            // SETVL = scheduled MW, VSCHD = scheduled kV
            let vd_i = dc.scheduled_voltage_kv.max(1.0); // inverter DC voltage
            let i_dc = dc.scheduled_setpoint / vd_i; // kA
            let vd_r = vd_i + i_dc * dc.resistance_ohm; // rectifier DC voltage (slightly higher)
            (i_dc, vd_r, vd_i)
        }
        LccHvdcControlMode::CurrentControl => {
            // SETVL = scheduled kA, VSCHD = scheduled kV
            let i_dc = dc.scheduled_setpoint;
            let vd_i = dc.scheduled_voltage_kv.max(1.0);
            let vd_r = vd_i + i_dc * dc.resistance_ohm;
            (i_dc, vd_r, vd_i)
        }
        LccHvdcControlMode::Blocked => return (0.0, 0.0, 0.0, 0.0),
    };

    // Rectifier AC power and reactive absorption
    // P_rect = Vd_r * Idc  (active power consumed from AC side)
    // Q_rect ≈ P_rect * tan(arccos(Vd_r / Vd0_r)) — simplified using commutation factor
    // Simplified: Q/P ratio ≈ sqrt(max(0, 1 - cos²)) / cos with cos = Vd_r/(Vd0_r)
    // For a fixed-schedule estimate we use the approximation: Q ≈ P * tan(15°) for
    // a typical operating point near α=15°. This is replaced by the exact value in
    // SequentialAcDc mode.
    let p_rect = vd_r_kv * i_dc_ka; // MW (kV * kA = MW)
    let p_inv = vd_i_kv * i_dc_ka; // MW (power delivered to inverter AC side)

    // Reactive: lagging Q ≈ P × tan(arccos(Vd / Vd0))
    // Approximation for typical HVDC angles: cos(α) ≈ Vd/Vd0, Vd0 ≈ Vd * (1+margin).
    // Use ~15° margin for rectifier (ALFMN=5°, typical operating 15–20°).
    let typical_cos_r = (15.0_f64.to_radians()).cos(); // cos(15°) ≈ 0.966
    let typical_cos_i = (20.0_f64.to_radians()).cos(); // cos(20°) ≈ 0.940 (inverter larger γ)
    let q_rect = p_rect * (1.0 - typical_cos_r * typical_cos_r).sqrt() / typical_cos_r;
    let q_inv = p_inv * (1.0 - typical_cos_i * typical_cos_i).sqrt() / typical_cos_i;

    (p_rect, q_rect, p_inv, q_inv)
}

fn apply_explicit_vsc_converters(
    net: &mut Network,
    dc_grids: &[surge_network::network::DcGrid],
    bus_map: &HashMap<u32, usize>,
) {
    for dc_grid in dc_grids {
        for converter in &dc_grid.converters {
            let Some(vsc) = converter.as_vsc() else {
                continue;
            };
            if !vsc.status {
                continue;
            }
            let Some(&bi) = bus_map.get(&vsc.ac_bus) else {
                continue;
            };
            {
                use surge_network::network::Load;
                let mut load = Load::new(vsc.ac_bus, -vsc.active_power_mw, 0.0);
                load.id = format!("__hvdc_dc_grid_conv_{}", vsc.ac_bus);
                net.loads.push(load);
            }
            if vsc.control_type_ac == 2 {
                let vs = vsc
                    .voltage_setpoint_pu
                    .clamp(vsc.voltage_min_pu, vsc.voltage_max_pu)
                    .max(0.5);
                net.buses[bi].bus_type = BusType::PV;
                net.buses[bi].voltage_magnitude_pu = vs;
                let mut vsc_gen = Generator::new(vsc.ac_bus, 0.0, vs);
                vsc_gen.qmin = vsc.reactive_power_ac_min_mvar;
                vsc_gen.qmax = vsc.reactive_power_ac_max_mvar;
                net.generators.push(vsc_gen);
            } else {
                let q = vsc.reactive_power_mvar.clamp(
                    vsc.reactive_power_ac_min_mvar,
                    vsc.reactive_power_ac_max_mvar,
                );
                {
                    use surge_network::network::Load;
                    let mut load = Load::new(vsc.ac_bus, 0.0, -q);
                    load.id = format!("__hvdc_dc_grid_conv_q2_{}", vsc.ac_bus);
                    net.loads.push(load);
                }
            }
        }
    }
}

/// Apply VSC fixed-schedule injections to the network.
fn apply_vsc_fixed_schedule(net: &mut Network, vsc: &VscHvdcLink, bus_map: &HashMap<u32, usize>) {
    // Converter 1 dc_setpoint is treated as the DC power schedule (MW) in PowerControl mode.
    // Losses = ALOSS + BLOSS * |P|.
    let p_dc = vsc.converter1.dc_setpoint.abs();
    let losses1 = vsc.converter1.loss_constant_mw + vsc.converter1.loss_linear * p_dc;
    let losses2 = vsc.converter2.loss_constant_mw + vsc.converter2.loss_linear * p_dc;

    let send_p = p_dc + losses1; // sending converter absorbs power from AC
    let recv_p = p_dc - losses2; // receiving converter injects (slightly less due to losses)

    use surge_network::network::Load;
    if bus_map.contains_key(&vsc.converter1.bus) {
        let mut load = Load::new(vsc.converter1.bus, send_p, 0.0);
        load.id = format!("__hvdc_vsc_conv1_{}", vsc.name);
        net.loads.push(load);
        match vsc.converter1.control_mode {
            VscConverterAcControlMode::ReactivePower => {
                let q1 = vsc
                    .converter1
                    .ac_setpoint
                    .clamp(vsc.converter1.q_min_mvar, vsc.converter1.q_max_mvar);
                let mut q_load = Load::new(vsc.converter1.bus, 0.0, -q1);
                q_load.id = format!("__hvdc_vsc_conv1_q_{}", vsc.name);
                net.loads.push(q_load);
            }
            VscConverterAcControlMode::AcVoltage => {
                if let Some(&bi) = bus_map.get(&vsc.converter1.bus) {
                    if net.buses[bi].bus_type != BusType::Slack {
                        net.buses[bi].bus_type = BusType::PV;
                    }
                    let vs = vsc
                        .converter1
                        .ac_setpoint
                        .clamp(vsc.converter1.voltage_min_pu, vsc.converter1.voltage_max_pu)
                        .max(0.5);
                    net.buses[bi].voltage_magnitude_pu = vs;
                    let mut vsc_gen = Generator::new(vsc.converter1.bus, 0.0, vs);
                    vsc_gen.qmin = vsc.converter1.q_min_mvar;
                    vsc_gen.qmax = vsc.converter1.q_max_mvar;
                    net.generators.push(vsc_gen);
                }
            }
        }
    }
    if bus_map.contains_key(&vsc.converter2.bus) {
        let mut load = Load::new(vsc.converter2.bus, -recv_p, 0.0);
        load.id = format!("__hvdc_vsc_conv2_{}", vsc.name);
        net.loads.push(load);
        match vsc.converter2.control_mode {
            VscConverterAcControlMode::ReactivePower => {
                let q2 = vsc
                    .converter2
                    .ac_setpoint
                    .clamp(vsc.converter2.q_min_mvar, vsc.converter2.q_max_mvar);
                let mut q_load = Load::new(vsc.converter2.bus, 0.0, -q2);
                q_load.id = format!("__hvdc_vsc_conv2_q_{}", vsc.name);
                net.loads.push(q_load);
            }
            VscConverterAcControlMode::AcVoltage => {
                if let Some(&bi) = bus_map.get(&vsc.converter2.bus) {
                    if net.buses[bi].bus_type != BusType::Slack {
                        net.buses[bi].bus_type = BusType::PV;
                    }
                    let vs = vsc
                        .converter2
                        .ac_setpoint
                        .clamp(vsc.converter2.voltage_min_pu, vsc.converter2.voltage_max_pu)
                        .max(0.5);
                    net.buses[bi].voltage_magnitude_pu = vs;
                    let mut vsc_gen = Generator::new(vsc.converter2.bus, 0.0, vs);
                    vsc_gen.qmin = vsc.converter2.q_min_mvar;
                    vsc_gen.qmax = vsc.converter2.q_max_mvar;
                    net.generators.push(vsc_gen);
                }
            }
        }
    }

    debug!(
        name = vsc.name,
        send_p, recv_p, "fixed-schedule VSC DC injection applied"
    );
}

// ---------------------------------------------------------------------------
// Sequential AC/DC outer loop
// ---------------------------------------------------------------------------

/// State of one LCC two-terminal DC line for the sequential AC/DC iteration.
#[derive(Debug, Clone)]
struct LccDcState {
    /// Index into the canonical `network.hvdc.links` vector.
    idx: usize,
    /// DC current in kA.
    i_dc: f64,
    /// Rectifier DC voltage in kV.
    vd_r: f64,
    /// Inverter DC voltage in kV.
    vd_i: f64,
    /// Rectifier firing angle (radians).
    alpha: f64,
    /// Inverter extinction angle (radians).
    gamma: f64,
    /// Rectifier AC power absorbed (MW).
    p_rect: f64,
    /// Rectifier reactive power absorbed (MVAr).
    q_rect: f64,
    /// Inverter AC power injected (MW).
    p_inv: f64,
    /// Inverter reactive power absorbed (MVAr).
    q_inv: f64,
}

impl LccDcState {
    /// Initialise from scheduled values (first guess).
    fn from_schedule(dc: &LccHvdcLink, idx: usize) -> Self {
        let (p_rect, q_rect, p_inv, q_inv) = compute_lcc_fixed_schedule(dc);
        let vd_i = dc.scheduled_voltage_kv.max(1.0);
        let i_dc = match dc.mode {
            LccHvdcControlMode::PowerControl => dc.scheduled_setpoint / vd_i,
            LccHvdcControlMode::CurrentControl => dc.scheduled_setpoint,
            LccHvdcControlMode::Blocked => 0.0,
        };
        let vd_r = vd_i + i_dc * dc.resistance_ohm;

        Self {
            idx,
            i_dc,
            vd_r,
            vd_i,
            alpha: 15.0_f64.to_radians(),
            gamma: 20.0_f64.to_radians(),
            p_rect,
            q_rect,
            p_inv,
            q_inv,
        }
    }

    /// Update DC operating point from AC bus voltages.
    ///
    /// Given the updated bus voltage magnitudes from the AC NR solution,
    /// recompute the DC circuit operating point.
    ///
    /// Reference: Anderson & Fouad, "Power System Control and Stability" ch. 9;
    /// also MATPOWER's `pdip_qdc.m` and `qdc.m`.
    fn update_from_ac(
        &mut self,
        dc: &LccHvdcLink,
        vm_r: f64,      // rectifier bus voltage magnitude (pu)
        vm_i: f64,      // inverter bus voltage magnitude (pu)
        base_kv_r: f64, // rectifier bus base kV
        base_kv_i: f64, // inverter bus base kV
    ) {
        if dc.mode == LccHvdcControlMode::Blocked {
            self.p_rect = 0.0;
            self.q_rect = 0.0;
            self.p_inv = 0.0;
            self.q_inv = 0.0;
            return;
        }

        let conv_r = &dc.rectifier;
        let conv_i = &dc.inverter;

        // Ideal no-load DC voltages (kV line-to-line on AC side of converter transformer):
        //   Vd0 = (3√2/π) × N_bridges × TR × TAP × |V_ac| × base_kV
        // TR (TRR/TRI) is the rated turns ratio; TAP (TAPR/TAPI) is the off-nominal tap.
        // Both must be included. base_kV is the AC bus nominal voltage (BASKV).
        let sqrt2_3_pi: f64 = 3.0 * 2.0_f64.sqrt() / PI;
        let vd0_r = sqrt2_3_pi
            * (conv_r.n_bridges as f64)
            * conv_r.turns_ratio
            * conv_r.tap
            * vm_r
            * base_kv_r;
        let vd0_i = sqrt2_3_pi
            * (conv_i.n_bridges as f64)
            * conv_i.turns_ratio
            * conv_i.tap
            * vm_i
            * base_kv_i;

        // Commutation-voltage-drop equivalent resistances (ohms):
        //   Rc = (3/π) × N_bridges × X_comm
        let three_over_pi = 3.0 / PI;
        let rc_r = three_over_pi * (conv_r.n_bridges as f64) * conv_r.commutation_reactance_ohm;
        let rc_i = three_over_pi * (conv_i.n_bridges as f64) * conv_i.commutation_reactance_ohm;

        // Angle limits in radians.
        let alpha_min_rad = conv_r.alpha_min.to_radians();
        let alpha_max_rad = conv_r.alpha_max.to_radians();
        let gamma_min_rad = conv_i.alpha_min.to_radians();
        let gamma_max_rad = conv_i.alpha_max.to_radians();

        // Solve DC circuit for the actual operating point given updated Vd0 values.
        //
        // PowerControl (MDC=1): inverter normally controls at γ_min (constant extinction angle).
        //   Vd_i = Vd0_i × cos(γ_min) − Rc_i × Idc
        //   Pdc  = Vd_i × Idc = SETVL
        //   → Rc_i × Idc² − Vd0_i × cos(γ_min) × Idc + SETVL = 0  (solve quadratic)
        //
        // CurrentControl (MDC=2): Idc = SETVL directly; inverter voltage follows.
        let cos_gmin = gamma_min_rad.cos();
        let i_dc = match dc.mode {
            LccHvdcControlMode::PowerControl => {
                if rc_i < 1e-9 {
                    // Zero commutating reactance: linear solution.
                    if vd0_i * cos_gmin > 1e-6 {
                        dc.scheduled_setpoint / (vd0_i * cos_gmin)
                    } else {
                        dc.scheduled_setpoint / dc.scheduled_voltage_kv.max(1.0)
                    }
                } else {
                    // Quadratic: Rc_i × Idc² − Vd0_i × cos(γ_min) × Idc + SETVL = 0
                    let discriminant =
                        (vd0_i * cos_gmin).powi(2) - 4.0 * rc_i * dc.scheduled_setpoint;
                    if discriminant >= 0.0 {
                        // Smaller root → higher Vd_i → stable operating point.
                        (vd0_i * cos_gmin - discriminant.sqrt()) / (2.0 * rc_i)
                    } else {
                        // No real solution (AC voltage too low); fall back to schedule.
                        dc.scheduled_setpoint / dc.scheduled_voltage_kv.max(1.0)
                    }
                }
            }
            LccHvdcControlMode::CurrentControl => dc.scheduled_setpoint,
            LccHvdcControlMode::Blocked => 0.0,
        };

        // Inverter DC voltage from circuit equation (power control: Vd_i = P/Idc).
        let vd_i = if dc.mode == LccHvdcControlMode::PowerControl && i_dc > 1e-9 {
            (vd0_i * cos_gmin - rc_i * i_dc).max(0.0)
        } else {
            (dc.scheduled_voltage_kv).max(1.0)
        };
        // Rectifier DC voltage = inverter voltage + resistive drop in DC cable.
        let vd_r = vd_i + i_dc * dc.resistance_ohm;

        // Firing angle α from rectifier equation: Vd_r = Vd0_r × cos(α) − Rc_r × Idc
        let cos_alpha = if vd0_r > 1e-6 {
            ((vd_r + rc_r * i_dc) / vd0_r).clamp(-1.0, 1.0)
        } else {
            1.0
        };
        let mut alpha = cos_alpha.acos().clamp(alpha_min_rad, alpha_max_rad);

        // Extinction angle γ from inverter equation: Vd_i = Vd0_i × cos(γ) − Rc_i × Idc
        let cos_gamma = if vd0_i > 1e-6 {
            ((vd_i + rc_i * i_dc) / vd0_i).clamp(-1.0, 1.0)
        } else {
            1.0
        };
        let mut gamma = cos_gamma.acos().clamp(gamma_min_rad, gamma_max_rad);

        // Clamp both angles once more after re-deriving (defensive).
        alpha = alpha.clamp(alpha_min_rad, alpha_max_rad);
        gamma = gamma.clamp(gamma_min_rad, gamma_max_rad);

        // Converter AC power and reactive absorption:
        //   P = Vd0 × cos(angle) × Idc
        //   Q = P × tan(φ) where cos(φ) = Vd / Vd0  (fundamental bridge power factor)
        let p_rect = vd0_r * alpha.cos() * i_dc;
        let cos_pf_r = if vd0_r > 1e-6 { vd_r / vd0_r } else { 1.0 }.clamp(0.0, 1.0);
        let q_rect = p_rect * (1.0 - cos_pf_r * cos_pf_r).sqrt() / cos_pf_r.max(1e-6);

        let p_inv = vd0_i * gamma.cos() * i_dc;
        let cos_pf_i = if vd0_i > 1e-6 { vd_i / vd0_i } else { 1.0 }.clamp(0.0, 1.0);
        let q_inv = p_inv * (1.0 - cos_pf_i * cos_pf_i).sqrt() / cos_pf_i.max(1e-6);

        self.i_dc = i_dc;
        self.vd_r = vd_r;
        self.vd_i = vd_i;
        self.alpha = alpha;
        self.gamma = gamma;
        self.p_rect = p_rect;
        self.q_rect = q_rect;
        self.p_inv = p_inv;
        self.q_inv = q_inv;
    }

    /// Maximum DC power change for convergence check.
    fn max_delta_p(&self, other: &Self) -> f64 {
        (self.p_rect - other.p_rect)
            .abs()
            .max((self.p_inv - other.p_inv).abs())
    }
}

// ---------------------------------------------------------------------------
// VSC sequential state
// ---------------------------------------------------------------------------

/// Per-iteration state for one VSC-HVDC link in the sequential AC/DC loop.
///
/// For `PowerControl` links, active power does not depend on AC bus voltages
/// (losses are affine in P, not in V), so `p1_mw` and `p2_mw` are constant
/// across outer iterations. The outer loop still matters for `AcVoltage`
/// Q-control mode: the converter bus is registered as a PV bus each iteration,
/// letting NR enforce the voltage setpoint and find the required Q within
/// `[q_min, q_max]`.
#[derive(Debug, Clone)]
struct VscDcState {
    /// Index into the canonical `network.hvdc.links` vector.
    idx: usize,
    /// Power absorbed by converter1 from its AC bus (MW, positive = load).
    p1_mw: f64,
    /// Power injected by converter2 into its AC bus (MW, positive = generation).
    p2_mw: f64,
}

impl VscDcState {
    /// Initialise from the scheduled DC setpoint.
    fn from_schedule(vsc: &VscHvdcLink, idx: usize) -> Self {
        if vsc.mode == VscHvdcControlMode::Blocked {
            return Self {
                idx,
                p1_mw: 0.0,
                p2_mw: 0.0,
            };
        }
        let p_dc = vsc.converter1.dc_setpoint.abs();
        let losses1 = vsc.converter1.loss_constant_mw + vsc.converter1.loss_linear * p_dc;
        let losses2 = vsc.converter2.loss_constant_mw + vsc.converter2.loss_linear * p_dc;
        Self {
            idx,
            p1_mw: p_dc + losses1,
            p2_mw: p_dc - losses2,
        }
    }

    /// Maximum active-power change for convergence check.
    ///
    /// For `PowerControl` VSC links this is always 0.0 — P does not depend on
    /// AC bus voltages so the outer loop converges trivially on the P criterion.
    /// The method exists so the same convergence check covers both LCC and VSC.
    fn max_delta_p(&self, other: &Self) -> f64 {
        (self.p1_mw - other.p1_mw)
            .abs()
            .max((self.p2_mw - other.p2_mw).abs())
    }
}

/// Run the sequential AC/DC outer loop.
fn solve_sequential_ac_dc(
    network: &Network,
    options: &AcPfOptions,
) -> Result<PfSolution, AcPfError> {
    let bus_map: HashMap<u32, usize> = network
        .buses
        .iter()
        .enumerate()
        .map(|(i, b)| (b.number, i))
        .collect();

    // Build bus base_kv lookup
    let bus_basekv: HashMap<u32, f64> = network
        .buses
        .iter()
        .map(|b| (b.number, b.base_kv))
        .collect();

    // Initialise DC states from scheduled values.
    let mut lcc_states: Vec<LccDcState> = network
        .hvdc
        .links
        .iter()
        .filter_map(|link| link.as_lcc())
        .enumerate()
        .map(|(i, dc)| LccDcState::from_schedule(dc, i))
        .collect();
    let vsc_states: Vec<VscDcState> = network
        .hvdc
        .links
        .iter()
        .filter_map(|link| link.as_vsc())
        .enumerate()
        .map(|(i, vsc)| VscDcState::from_schedule(vsc, i))
        .collect();

    let mut solution = Err(AcPfError::NotConverged {
        iterations: 0,
        max_mismatch: f64::INFINITY,
        worst_bus: None,
        partial_vm: None,
        partial_va: None,
    });
    let max_iter = options.dc_max_iter;
    let tol_mw = options.dc_tol_mw;

    for outer in 0..max_iter {
        // Build network with current DC injections (LCC + VSC).
        let modified = inject_dc_states(network, &lcc_states, &vsc_states, &bus_map);

        // Solve AC network.
        solution = solve_ac_pf_kernel(&modified, options);
        let sol = match &solution {
            Ok(s) => s,
            Err(_) => break,
        };

        // Update LCC states from new bus voltages.
        let prev_lcc = lcc_states.clone();
        for (i, dc) in network
            .hvdc
            .links
            .iter()
            .filter_map(|link| link.as_lcc())
            .enumerate()
        {
            let vm_r = bus_map
                .get(&dc.rectifier.bus)
                .map(|&idx| sol.voltage_magnitude_pu[idx])
                .unwrap_or(1.0);
            let vm_i = bus_map
                .get(&dc.inverter.bus)
                .map(|&idx| sol.voltage_magnitude_pu[idx])
                .unwrap_or(1.0);
            let base_kv_r = bus_basekv.get(&dc.rectifier.bus).copied().unwrap_or(1.0);
            let base_kv_i = bus_basekv.get(&dc.inverter.bus).copied().unwrap_or(1.0);
            lcc_states[i].update_from_ac(dc, vm_r, vm_i, base_kv_r, base_kv_i);
        }

        // VSC state: PowerControl P is voltage-independent — no update needed.
        // VdcControl is rejected earlier by `reject_unsupported_vsc_modes`.
        let prev_vsc = vsc_states.clone();

        // Convergence: max delta across LCC and VSC.
        let max_delta: f64 = lcc_states
            .iter()
            .zip(prev_lcc.iter())
            .map(|(new, old)| new.max_delta_p(old))
            .chain(
                vsc_states
                    .iter()
                    .zip(prev_vsc.iter())
                    .map(|(new, old)| new.max_delta_p(old)),
            )
            .fold(0.0_f64, f64::max);

        info!(
            outer,
            max_delta_mw = max_delta,
            "AC/DC sequential iteration"
        );

        if max_delta < tol_mw {
            debug!(outer, "AC/DC sequential converged");
            break;
        }

        if outer == max_iter - 1 {
            warn!(
                max_delta_mw = max_delta,
                max_iter,
                "AC/DC sequential did not converge to {tol_mw} MW in {max_iter} iterations"
            );
        }
    }

    solution
}

/// Build a network clone with DC state injections applied to bus P/Q.
///
/// LCC lines: apply P and Q from the iterated [`LccDcState`].
///
/// VSC lines:
/// - `ReactivePower` Q-control: inject scheduled Q as a constant load.
/// - `AcVoltage` control: promote the converter bus to PV and add a
///   synthetic generator with the voltage setpoint and Q-capability limits,
///   letting Newton-Raphson hold the terminal voltage and solve for Q.
fn inject_dc_states(
    network: &Network,
    lcc_states: &[LccDcState],
    vsc_states: &[VscDcState],
    bus_map: &HashMap<u32, usize>,
) -> Network {
    let mut net = network.clone();

    // LCC injections.
    for state in lcc_states {
        let dc = network
            .hvdc
            .links
            .iter()
            .filter_map(|link| link.as_lcc())
            .nth(state.idx)
            .expect("LCC state index must reference a point-to-point LCC link");
        if dc.mode == LccHvdcControlMode::Blocked {
            continue;
        }
        {
            use surge_network::network::Load;
            if bus_map.contains_key(&dc.rectifier.bus) {
                let mut load = Load::new(dc.rectifier.bus, state.p_rect, state.q_rect);
                load.id = format!("__hvdc_iter_lcc_rect_{}", state.idx);
                net.loads.push(load);
            }
            if bus_map.contains_key(&dc.inverter.bus) {
                let mut load = Load::new(dc.inverter.bus, -state.p_inv, state.q_inv);
                load.id = format!("__hvdc_iter_lcc_inv_{}", state.idx);
                net.loads.push(load);
            }
        }
    }

    // VSC injections — mode-aware.
    for state in vsc_states {
        let vsc = network
            .hvdc
            .links
            .iter()
            .filter_map(|link| link.as_vsc())
            .nth(state.idx)
            .expect("VSC state index must reference a point-to-point VSC link");
        if vsc.mode == VscHvdcControlMode::Blocked {
            continue;
        }

        // Helper: apply one converter terminal to the cloned network.
        // `absorbs` true  → terminal draws p_mw from AC bus (pd += p_mw).
        // `absorbs` false → terminal injects p_mw into AC bus (pd -= p_mw).
        let apply_converter = |net: &mut Network,
                               conv: &surge_network::network::VscConverterTerminal,
                               p_mw: f64,
                               absorbs: bool| {
            let Some(&bi) = bus_map.get(&conv.bus) else {
                return;
            };
            {
                use surge_network::network::Load;
                let pd = if absorbs { p_mw } else { -p_mw };
                let mut load = Load::new(conv.bus, pd, 0.0);
                load.id = format!("__hvdc_iter_vsc_p_{}", conv.bus);
                net.loads.push(load);
            }
            match conv.control_mode {
                VscConverterAcControlMode::ReactivePower => {
                    // Fixed reactive setpoint: positive = injection into AC.
                    let q = conv.ac_setpoint.clamp(conv.q_min_mvar, conv.q_max_mvar);
                    {
                        use surge_network::network::Load;
                        let mut load = Load::new(conv.bus, 0.0, -q);
                        load.id = format!("__hvdc_iter_vsc_q_{}", conv.bus);
                        net.loads.push(load);
                    }
                }
                VscConverterAcControlMode::AcVoltage => {
                    // Voltage-regulating: promote to PV, add synthetic generator
                    // so NR enforces the terminal voltage and respects Q limits.
                    let vs = conv
                        .ac_setpoint
                        .clamp(conv.voltage_min_pu, conv.voltage_max_pu)
                        .max(0.5);
                    net.buses[bi].bus_type = BusType::PV;
                    net.buses[bi].voltage_magnitude_pu = vs;
                    let mut vsc_gen = Generator::new(conv.bus, 0.0, vs);
                    vsc_gen.qmin = conv.q_min_mvar;
                    vsc_gen.qmax = conv.q_max_mvar;
                    net.generators.push(vsc_gen);
                }
            }
        };

        apply_converter(&mut net, &vsc.converter1, state.p1_mw, true);
        apply_converter(&mut net, &vsc.converter2, state.p2_mw, false);
    }

    // Explicit DC-grid VSC converters — P is constant, same as fixed-schedule.
    apply_explicit_vsc_converters(&mut net, &network.hvdc.dc_grids, bus_map);

    net
}

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Generator, Load};
    use surge_network::network::{Bus, BusType};
    use surge_network::network::{LccConverterTerminal, LccHvdcControlMode, LccHvdcLink};
    use surge_network::network::{VscConverterTerminal, VscHvdcControlMode, VscHvdcLink};

    fn make_3bus_base() -> Network {
        let mut net = Network::new("test-3bus");
        let b1 = Bus::new(1, BusType::Slack, 345.0);
        let b2 = Bus::new(2, BusType::PQ, 345.0);
        let b3 = Bus::new(3, BusType::PV, 345.0);
        net.buses.extend([b1, b2, b3]);
        net.loads.push(Load::new(2, 200.0, 0.0));
        net.loads.push(Load::new(3, 100.0, 0.0));
        net.branches.push(Branch::new_line(1, 2, 0.005, 0.05, 0.04));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.02));
        net.generators.push(Generator::new(1, 300.0, 1.0));
        net.generators.push(Generator::new(3, 100.0, 1.0));
        net
    }

    #[test]
    fn test_solve_ac_pf_with_dc_lines_no_dc_lines_delegates_to_nr() {
        let net = make_3bus_base();
        let opts = AcPfOptions::default();
        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(
            result.is_ok(),
            "solve_ac_pf_with_dc_lines should succeed for base case without DC lines"
        );
    }

    #[test]
    fn test_solve_ac_pf_with_dc_lines_fixed_schedule_lcc() {
        let mut net = make_3bus_base();

        // Add a 100 MW LCC DC line from bus 1 (rectifier) to bus 3 (inverter)
        let dc = LccHvdcLink {
            name: "DC1".into(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 1.0,
            scheduled_setpoint: 100.0,   // 100 MW scheduled
            scheduled_voltage_kv: 345.0, // scheduled DC voltage
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 5.0,
                commutation_resistance_ohm: 0.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 3,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 17.0,
                commutation_resistance_ohm: 0.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        };
        net.hvdc.push_lcc_link(dc);

        let opts = AcPfOptions::default();
        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(
            result.is_ok(),
            "solve_ac_pf_with_dc_lines with LCC DC line should converge"
        );

        let sol = result.unwrap();
        // Bus voltages should be finite and in a reasonable range
        for vm in &sol.voltage_magnitude_pu {
            assert!(*vm > 0.5 && *vm < 1.5, "bus voltage {vm} out of range");
        }
    }

    #[test]
    fn test_solve_ac_pf_with_dc_lines_fixed_schedule_vsc() {
        let mut net = make_3bus_base();

        // Add a 50 MW VSC DC link from bus 1 to bus 2
        let vsc = VscHvdcLink {
            name: "VSC1".into(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            converter1: VscConverterTerminal {
                bus: 1,
                dc_setpoint: 50.0, // 50 MW scheduled
                ac_setpoint: 0.0,  // no reactive setpoint
                loss_constant_mw: 1.0,
                loss_linear: 0.02,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                dc_setpoint: -50.0,
                ac_setpoint: 0.0,
                loss_constant_mw: 1.0,
                loss_linear: 0.02,
                in_service: true,
                ..VscConverterTerminal::default()
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let opts = AcPfOptions::default();
        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(
            result.is_ok(),
            "solve_ac_pf_with_dc_lines with VSC DC line should converge"
        );
    }

    #[test]
    fn test_fixed_schedule_vsc_ac_voltage_mode_is_voltage_regulating() {
        use surge_network::network::VscConverterAcControlMode;

        let mut net = make_3bus_base();
        let vsc = VscHvdcLink {
            name: "VSC-ACV".into(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.2,
            converter1: VscConverterTerminal {
                bus: 1,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 40.0,
                ac_setpoint: 0.0,
                q_min_mvar: -50.0,
                q_max_mvar: 50.0,
                loss_constant_mw: 1.0,
                loss_linear: 0.0,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                control_mode: VscConverterAcControlMode::AcVoltage,
                dc_setpoint: -40.0,
                ac_setpoint: 0.98,
                q_min_mvar: -100.0,
                q_max_mvar: 100.0,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                loss_constant_mw: 1.0,
                loss_linear: 0.0,
                in_service: true,
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let modified = inject_fixed_schedule_dc(&net);
        assert_eq!(modified.buses[1].bus_type, BusType::PV);
        assert!((modified.buses[1].voltage_magnitude_pu - 0.98).abs() < 1e-12);
        let q_sum: f64 = modified
            .loads
            .iter()
            .filter(|load| load.id.starts_with("__hvdc_vsc_conv2_"))
            .map(|load| load.reactive_power_demand_mvar)
            .sum();
        assert!(
            q_sum.abs() < 1e-12,
            "AcVoltage converter should not be converted into a fixed Q load"
        );
    }

    #[test]
    fn test_fixed_schedule_vsc_receiving_end_does_not_clamp_losses() {
        let mut net = make_3bus_base();
        let vsc = VscHvdcLink {
            name: "VSC-LOSS".into(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.2,
            converter1: VscConverterTerminal {
                bus: 1,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 1.0,
                ac_setpoint: 0.0,
                loss_constant_mw: 5.0,
                loss_linear: 0.0,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: -1.0,
                ac_setpoint: 0.0,
                loss_constant_mw: 5.0,
                loss_linear: 0.0,
                in_service: true,
                ..VscConverterTerminal::default()
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let modified = inject_fixed_schedule_dc(&net);
        let recv_load = modified
            .loads
            .iter()
            .find(|load| load.id == "__hvdc_vsc_conv2_VSC-LOSS")
            .expect("receiving-end converter load should be present");
        assert!(
            recv_load.active_power_demand_mw > 0.0,
            "receiving-end load should include converter losses instead of clamping to zero"
        );
    }

    #[test]
    fn test_vdc_control_is_rejected_by_ac_pf() {
        let mut net = make_3bus_base();
        let vsc = VscHvdcLink {
            name: "VSC-VDC".into(),
            mode: VscHvdcControlMode::VdcControl,
            resistance_ohm: 0.2,
            converter1: VscConverterTerminal {
                bus: 1,
                dc_setpoint: 40.0,
                ac_setpoint: 0.0,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                dc_setpoint: -40.0,
                ac_setpoint: 0.0,
                in_service: true,
                ..VscConverterTerminal::default()
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let err = solve_ac_pf_with_dc_lines(&net, &AcPfOptions::default()).unwrap_err();
        match err {
            AcPfError::InvalidOptions(msg) => {
                assert!(msg.contains("VdcControl"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidOptions for unsupported VdcControl, got {other:?}"),
        }
    }

    #[test]
    fn test_sequential_ac_dc_converges() {
        let mut net = make_3bus_base();

        let dc = LccHvdcLink {
            name: "DC1".into(),
            mode: LccHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            scheduled_setpoint: 100.0,
            scheduled_voltage_kv: 345.0,
            rectifier: LccConverterTerminal {
                bus: 1,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 5.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            inverter: LccConverterTerminal {
                bus: 3,
                n_bridges: 2,
                alpha_max: 80.0,
                alpha_min: 17.0,
                commutation_reactance_ohm: 5.0,
                base_voltage_kv: 345.0,
                tap: 1.0,
                ..LccConverterTerminal::default()
            },
            ..LccHvdcLink::default()
        };
        net.hvdc.push_lcc_link(dc);

        let opts = AcPfOptions {
            dc_line_model: DcLineModel::SequentialAcDc,
            dc_max_iter: 25,
            dc_tol_mw: 1.0,
            ..Default::default()
        };

        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(result.is_ok(), "sequential AC/DC should converge");

        let sol = result.unwrap();
        for vm in &sol.voltage_magnitude_pu {
            assert!(
                *vm > 0.5 && *vm < 1.5,
                "voltage {vm} out of range after AC/DC iteration"
            );
        }
    }

    #[test]
    fn test_sequential_vsc_reactive_power_mode() {
        // SequentialAcDc with a ReactivePower-mode VSC: Q is the fixed
        // ac_setpoint clamped to [q_min, q_max].  Solver should converge and
        // all bus voltages should be in a plausible range.
        use surge_network::network::VscConverterAcControlMode;

        let mut net = make_3bus_base();
        let vsc = VscHvdcLink {
            name: "VSC-RPM".into(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.5,
            converter1: VscConverterTerminal {
                bus: 1,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 60.0,
                ac_setpoint: 15.0, // inject 15 MVAr into bus 1
                q_min_mvar: -50.0,
                q_max_mvar: 50.0,
                loss_constant_mw: 1.0,
                loss_linear: 0.01,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: -60.0,
                ac_setpoint: -10.0, // absorb 10 MVAr from bus 2
                q_min_mvar: -50.0,
                q_max_mvar: 50.0,
                loss_constant_mw: 1.0,
                loss_linear: 0.01,
                in_service: true,
                ..VscConverterTerminal::default()
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let opts = AcPfOptions {
            dc_line_model: DcLineModel::SequentialAcDc,
            dc_max_iter: 25,
            dc_tol_mw: 1.0,
            ..Default::default()
        };
        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(
            result.is_ok(),
            "sequential VSC ReactivePower mode should converge"
        );
        let sol = result.unwrap();
        for vm in &sol.voltage_magnitude_pu {
            assert!(
                *vm > 0.5 && *vm < 1.5,
                "voltage {vm} out of plausible range"
            );
        }
    }

    #[test]
    fn test_sequential_vsc_ac_voltage_mode() {
        // SequentialAcDc with an AcVoltage-mode VSC: converter bus is promoted
        // to PV and NR holds the terminal voltage at the setpoint.
        use surge_network::network::VscConverterAcControlMode;

        let mut net = make_3bus_base();

        // Bus 2 starts as PQ with a 200 MW load.  The VSC converter1 absorbs
        // 80 MW from bus 1 (slack) and the AcVoltage-mode converter2 on bus 2
        // regulates that bus to 0.98 pu.
        let vsc = VscHvdcLink {
            name: "VSC-ACV".into(),
            mode: VscHvdcControlMode::PowerControl,
            resistance_ohm: 0.3,
            converter1: VscConverterTerminal {
                bus: 1,
                control_mode: VscConverterAcControlMode::ReactivePower,
                dc_setpoint: 80.0,
                ac_setpoint: 0.0,
                q_min_mvar: -100.0,
                q_max_mvar: 100.0,
                loss_constant_mw: 1.5,
                loss_linear: 0.015,
                in_service: true,
                ..VscConverterTerminal::default()
            },
            converter2: VscConverterTerminal {
                bus: 2,
                control_mode: VscConverterAcControlMode::AcVoltage,
                dc_setpoint: -80.0,
                ac_setpoint: 0.98, // hold bus 2 at 0.98 pu
                q_min_mvar: -120.0,
                q_max_mvar: 120.0,
                loss_constant_mw: 1.5,
                loss_linear: 0.015,
                voltage_min_pu: 0.9,
                voltage_max_pu: 1.1,
                in_service: true,
            },
        };
        net.hvdc.push_vsc_link(vsc);

        let opts = AcPfOptions {
            dc_line_model: DcLineModel::SequentialAcDc,
            dc_max_iter: 25,
            dc_tol_mw: 1.0,
            enforce_q_limits: true,
            ..Default::default()
        };
        let result = solve_ac_pf_with_dc_lines(&net, &opts);
        assert!(
            result.is_ok(),
            "sequential VSC AcVoltage mode should converge"
        );

        let sol = result.unwrap();
        // Bus 2 (index 1) should be regulated close to 0.98 pu.
        let vm_bus2 = sol.voltage_magnitude_pu[1];
        assert!(
            (vm_bus2 - 0.98).abs() < 0.01,
            "bus 2 voltage {vm_bus2:.4} should be ~0.98 pu (AcVoltage setpoint)"
        );
        for vm in &sol.voltage_magnitude_pu {
            assert!(
                *vm > 0.5 && *vm < 1.5,
                "voltage {vm} out of plausible range"
            );
        }
    }
}
