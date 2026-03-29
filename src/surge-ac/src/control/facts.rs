// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! FACTS device control for Newton-Raphson power flow.
//!
//! Replaces the crude `expand_facts()` approach (which converted FACTS devices
//! into generators and branch modifications) with proper control modeling:
//!
//! - **SVC/STATCOM**: Variable susceptance `b_svc` with voltage regulation.
//!   Solved via outer-loop proportional update after NR convergence.
//! - **TCSC**: Variable series reactance `x_comp` with power flow control.
//!   Solved via outer-loop Newton step after NR convergence.
//! - **UPFC**: Combined shunt + series control (both SVC + TCSC entries).
//!
//! The outer-loop approach modifies Y-bus between NR solves, analogous to
//! OLTC/switched-shunt discrete control.  At susceptance/reactance limits,
//! the device locks to its limit value and ceases regulation.

use std::collections::HashMap;

use surge_network::Network;

use crate::matrix::ybus::YBus;

// ---------------------------------------------------------------------------
// SVC (shunt) control
// ---------------------------------------------------------------------------

/// SVC/STATCOM shunt voltage regulation state.
#[derive(Debug, Clone)]
pub struct SvcState {
    /// Internal bus index.
    pub bus_idx: usize,
    /// Current susceptance in per-unit on system base.
    pub b_svc: f64,
    /// Voltage setpoint (p.u.).
    pub voltage_setpoint_pu: f64,
    /// Maximum susceptance (capacitive, positive, p.u.).
    pub b_max: f64,
    /// Minimum susceptance (inductive, negative, p.u.).
    pub b_min: f64,
    /// True when locked at a susceptance limit (no voltage regulation).
    pub at_limit: bool,
}

// ---------------------------------------------------------------------------
// TCSC (series) control
// ---------------------------------------------------------------------------

/// TCSC series reactance control state.
#[derive(Debug, Clone)]
pub struct TcscState {
    /// Internal bus index for the from-bus of the controlled branch.
    pub from_idx: usize,
    /// Internal bus index for the to-bus.
    pub to_idx: usize,
    /// Original branch reactance (before compensation).
    pub x_orig: f64,
    /// Branch resistance.
    pub r: f64,
    /// Current compensating reactance (subtracted from `x_orig`).
    pub x_comp: f64,
    /// Desired active power flow on the branch (p.u.).
    pub p_setpoint_mw: f64,
    /// Maximum compensation reactance.
    pub x_comp_max: f64,
    /// Minimum compensation reactance.
    pub x_comp_min: f64,
    /// Effective tap ratio of the branch.
    pub tap: f64,
    /// Phase shift in radians.
    pub shift_rad: f64,
    /// True when locked at a compensation limit.
    pub at_limit: bool,
}

// ---------------------------------------------------------------------------
// Collected FACTS control state
// ---------------------------------------------------------------------------

/// Collection of FACTS control states for the NR solver.
#[derive(Debug, Clone)]
pub struct FactsStates {
    /// SVC/STATCOM shunt device states.
    pub svcs: Vec<SvcState>,
    /// TCSC series compensator states.
    pub tcscs: Vec<TcscState>,
}

impl FactsStates {
    /// Initialize FACTS control states from the network.
    ///
    /// Builds SVC states for shunt devices and TCSC states for series devices.
    /// UPFC devices produce both an SVC and a TCSC entry.
    pub fn new(network: &Network, bus_map: &HashMap<u32, usize>) -> Self {
        let base_mva = network.base_mva;
        let mut svcs = Vec::new();
        let mut tcscs = Vec::new();

        for facts in &network.facts_devices {
            if !facts.mode.in_service() {
                continue;
            }

            let Some(&bus_i_idx) = bus_map.get(&facts.bus_from) else {
                continue;
            };

            // Shunt device (SVC/STATCOM/UPFC shunt part)
            if facts.mode.has_shunt() {
                let b_max = facts.q_max / base_mva;
                let b_min = -facts.q_max / base_mva;
                let b_init = (facts.q_setpoint_mvar / base_mva).clamp(b_min, b_max);

                svcs.push(SvcState {
                    bus_idx: bus_i_idx,
                    b_svc: b_init,
                    voltage_setpoint_pu: facts.voltage_setpoint_pu,
                    b_max,
                    b_min,
                    at_limit: false,
                });
            }

            // Series device (TCSC/UPFC series part)
            if facts.mode.has_series() && facts.bus_to > 0 {
                let Some(&_bus_j_idx) = bus_map.get(&facts.bus_to) else {
                    continue;
                };

                // Find the matching in-service branch.
                let branch_match = network.branches.iter().find(|br| {
                    br.in_service
                        && ((br.from_bus == facts.bus_from && br.to_bus == facts.bus_to)
                            || (br.from_bus == facts.bus_to && br.to_bus == facts.bus_from))
                });

                if let Some(br) = branch_match {
                    let from_idx = *bus_map.get(&br.from_bus).unwrap();
                    let to_idx = *bus_map.get(&br.to_bus).unwrap();

                    // Compensation limits: allow up to 80% of original reactance
                    // reduction and 50% increase (standard TCSC operating range).
                    let x_max = br.x.abs() * 0.8;
                    let x_min = -br.x.abs() * 0.5;

                    tcscs.push(TcscState {
                        from_idx,
                        to_idx,
                        x_orig: br.x,
                        r: br.r,
                        x_comp: facts.series_reactance_pu.clamp(x_min, x_max),
                        p_setpoint_mw: facts.p_setpoint_mw / base_mva,
                        x_comp_max: x_max,
                        x_comp_min: x_min,
                        tap: br.effective_tap(),
                        shift_rad: br.phase_shift_rad,
                        at_limit: false,
                    });
                } else {
                    tracing::warn!(
                        device = facts.name,
                        bus_from = facts.bus_from,
                        bus_to = facts.bus_to,
                        "FACTS series device: no matching branch found"
                    );
                }
            }
        }

        FactsStates { svcs, tcscs }
    }

    /// Returns true if there are no active FACTS devices.
    pub fn is_empty(&self) -> bool {
        self.svcs.is_empty() && self.tcscs.is_empty()
    }

    /// Apply initial SVC susceptances to the Y-bus.
    pub fn apply_svc_to_ybus(&self, ybus: &mut YBus) {
        for svc in &self.svcs {
            if svc.b_svc.abs() > 1e-15 {
                ybus.add_delta(svc.bus_idx, svc.bus_idx, 0.0, svc.b_svc);
            }
        }
    }

    /// Apply initial TCSC reactance compensation to the Y-bus.
    ///
    /// Computes Y-bus deltas for the reactance change `x_orig → x_orig - x_comp`
    /// and applies them.  Uses `tcsc_ybus_delta_from_base` to correctly compute
    /// the delta relative to the uncompensated branch.
    pub fn apply_tcsc_to_ybus(&self, ybus: &mut YBus) {
        for tcsc in &self.tcscs {
            if tcsc.x_comp.abs() < 1e-15 {
                continue;
            }
            let deltas = tcsc_ybus_delta_from_base(tcsc, 0.0, tcsc.x_comp);
            ybus.apply_deltas(&deltas);
        }
    }

    /// Update SVC susceptances after NR convergence (outer loop).
    ///
    /// For each active SVC, computes the susceptance adjustment:
    ///   ΔB = −v_err · B_kk / V_k²
    /// where B_kk is the Y-bus diagonal susceptance at the SVC bus (a
    /// network-derived sensitivity estimate for ∂Q/∂V).
    /// Step is damped to 30% of the full range per iteration.
    ///
    /// Returns the number of devices that changed susceptance.
    pub fn update_svc_susceptances(&mut self, vm: &[f64], ybus: &mut YBus) -> usize {
        let mut n_changed = 0;

        for svc in &mut self.svcs {
            if svc.at_limit {
                continue;
            }

            let vk = vm[svc.bus_idx];
            let v_err = vk - svc.voltage_setpoint_pu;

            if v_err.abs() < 1e-6 {
                continue; // Already at setpoint.
            }

            // ΔB = −v_err · |B_kk| / V_k²
            // B_kk (Y-bus diagonal susceptance) is a network-derived estimate of
            // ∂Q/∂V sensitivity at the bus.  Using it as the gain factor adapts
            // the step size to the actual network strength at each SVC bus.
            let vk_sq = vk * vk;
            let b_kk = ybus.b(svc.bus_idx, svc.bus_idx).abs().max(1.0);
            let delta_b = if vk_sq > 1e-6 {
                -v_err * b_kk / vk_sq
            } else {
                0.0
            };

            // Damping: limit step.
            let max_step = 0.3 * (svc.b_max - svc.b_min);
            let delta_b = delta_b.clamp(-max_step, max_step);

            let b_unclamped = svc.b_svc + delta_b;
            let b_new = b_unclamped.clamp(svc.b_min, svc.b_max);
            let actual_delta = b_new - svc.b_svc;

            if actual_delta.abs() < 1e-12 {
                continue;
            }

            ybus.add_delta(svc.bus_idx, svc.bus_idx, 0.0, actual_delta);
            svc.b_svc = b_new;
            n_changed += 1;

            // Lock at limit if the unclamped step would have exceeded bounds
            // AND the voltage error is driving it further out of range.
            if (b_unclamped > svc.b_max && v_err < 0.0) || (b_unclamped < svc.b_min && v_err > 0.0)
            {
                svc.at_limit = true;
                tracing::debug!(
                    bus = svc.bus_idx,
                    b_svc = svc.b_svc,
                    "SVC locked at susceptance limit"
                );
            }
        }

        n_changed
    }

    /// Update TCSC reactances after NR convergence (outer loop).
    ///
    /// Computes branch power flow and adjusts x_comp toward p_des.
    /// Returns the number of devices that changed compensation.
    pub fn update_tcsc_reactances(&mut self, vm: &[f64], va: &[f64], ybus: &mut YBus) -> usize {
        let mut n_changed = 0;

        for tcsc in &mut self.tcscs {
            if tcsc.at_limit || tcsc.p_setpoint_mw.abs() < 1e-12 {
                continue;
            }

            let f = tcsc.from_idx;
            let t = tcsc.to_idx;
            let x_eff = tcsc.x_orig - tcsc.x_comp;
            let z_sq = tcsc.r * tcsc.r + x_eff * x_eff;
            if z_sq < 1e-12 {
                continue;
            }

            let g_s = tcsc.r / z_sq;
            let b_s = -x_eff / z_sq;
            let tap = tcsc.tap;
            let tap_sq = tap * tap;
            let cos_s = tcsc.shift_rad.cos();
            let sin_s = tcsc.shift_rad.sin();

            let vf = vm[f];
            let vt = vm[t];
            let theta_ft = va[f] - va[t];
            let cos_ft = theta_ft.cos();
            let sin_ft = theta_ft.sin();

            let g_ff = g_s / tap_sq;
            let g_ft = -(g_s * cos_s + b_s * sin_s) / tap;
            let b_ft = -(b_s * cos_s - g_s * sin_s) / tap;
            let p_ft = vf * vf * g_ff + vf * vt * (g_ft * cos_ft + b_ft * sin_ft);

            let p_err = p_ft - tcsc.p_setpoint_mw;
            if p_err.abs() < 1e-6 {
                continue;
            }

            // Sensitivity ∂P_ft/∂x_comp (analytical).
            let dg = 2.0 * tcsc.r * x_eff / (z_sq * z_sq);
            let db = (tcsc.r * tcsc.r - x_eff * x_eff) / (z_sq * z_sq);
            let dg_ff = dg / tap_sq;
            let dg_ft_s = -(dg * cos_s + db * sin_s) / tap;
            let db_ft_s = -(db * cos_s - dg * sin_s) / tap;
            let dp_dx = vf * vf * dg_ff + vf * vt * (dg_ft_s * cos_ft + db_ft_s * sin_ft);

            if dp_dx.abs() < 1e-12 {
                continue;
            }

            let mut delta_x = -p_err / dp_dx;

            // Damping.
            let max_step = 0.3 * (tcsc.x_comp_max - tcsc.x_comp_min);
            delta_x = delta_x.clamp(-max_step, max_step);

            let x_unclamped = tcsc.x_comp + delta_x;
            let x_new = x_unclamped.clamp(tcsc.x_comp_min, tcsc.x_comp_max);
            let actual_delta = x_new - tcsc.x_comp;

            if actual_delta.abs() < 1e-12 {
                continue;
            }

            // Apply Y-bus delta.
            let deltas = tcsc_ybus_delta(tcsc, actual_delta);
            ybus.apply_deltas(&deltas);
            tcsc.x_comp = x_new;
            n_changed += 1;

            // Lock at limit if the unclamped step would have exceeded bounds.
            if x_unclamped > tcsc.x_comp_max || x_unclamped < tcsc.x_comp_min {
                tcsc.at_limit = true;
                tracing::debug!(
                    from = tcsc.from_idx,
                    to = tcsc.to_idx,
                    x_comp = tcsc.x_comp,
                    "TCSC locked at compensation limit"
                );
            }
        }

        n_changed
    }

    /// Check SVC susceptance limits after NR convergence.
    ///
    /// Returns the number of SVCs that switched to limit mode. If nonzero,
    /// the caller should re-solve with the clamped susceptance.
    pub fn check_svc_limits(&mut self, ybus: &mut YBus) -> usize {
        let mut n_changed = 0;
        for svc in &mut self.svcs {
            if svc.at_limit {
                continue;
            }
            if svc.b_svc > svc.b_max {
                let excess = svc.b_svc - svc.b_max;
                ybus.add_delta(svc.bus_idx, svc.bus_idx, 0.0, -excess);
                svc.b_svc = svc.b_max;
                svc.at_limit = true;
                n_changed += 1;
                tracing::debug!(
                    bus = svc.bus_idx,
                    b_svc = svc.b_svc,
                    "SVC hit max susceptance limit"
                );
            } else if svc.b_svc < svc.b_min {
                let deficit = svc.b_min - svc.b_svc;
                ybus.add_delta(svc.bus_idx, svc.bus_idx, 0.0, deficit);
                svc.b_svc = svc.b_min;
                svc.at_limit = true;
                n_changed += 1;
                tracing::debug!(
                    bus = svc.bus_idx,
                    b_svc = svc.b_svc,
                    "SVC hit min susceptance limit"
                );
            }
        }
        n_changed
    }

    /// Check TCSC compensation limits after NR convergence.
    pub fn check_tcsc_limits(&mut self, ybus: &mut YBus) -> usize {
        let mut n_changed = 0;
        for tcsc in &mut self.tcscs {
            if tcsc.at_limit {
                continue;
            }
            let clamped = tcsc.x_comp.clamp(tcsc.x_comp_min, tcsc.x_comp_max);
            if (clamped - tcsc.x_comp).abs() > 1e-12 {
                // Undo the excess compensation in Y-bus.
                let excess = tcsc.x_comp - clamped;
                let ybus_deltas = tcsc_ybus_delta(tcsc, -excess);
                ybus.apply_deltas(&ybus_deltas);
                tcsc.x_comp = clamped;
                tcsc.at_limit = true;
                n_changed += 1;
                tracing::debug!(
                    from = tcsc.from_idx,
                    to = tcsc.to_idx,
                    x_comp = tcsc.x_comp,
                    "TCSC hit compensation limit"
                );
            }
        }
        n_changed
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute Y-bus deltas for a TCSC reactance change from `x_comp_old` to `x_comp_new`.
///
/// The branch effective reactance changes from `x_orig − x_comp_old` to
/// `x_orig − x_comp_new`.  Returns 4 `(row, col, ΔG, ΔB)` entries for ff, tt, ft, tf.
fn tcsc_ybus_delta_from_base(
    tcsc: &TcscState,
    x_comp_old: f64,
    x_comp_new: f64,
) -> [(usize, usize, f64, f64); 4] {
    let r = tcsc.r;
    let tap = tcsc.tap;
    let tap_sq = tap * tap;
    let cos_s = tcsc.shift_rad.cos();
    let sin_s = tcsc.shift_rad.sin();

    // Old series admittance.
    let x_old = tcsc.x_orig - x_comp_old;
    let z_sq_old = r * r + x_old * x_old;
    let g_old = if z_sq_old < 1e-12 { 1e6 } else { r / z_sq_old };
    let b_old = if z_sq_old < 1e-12 {
        -1e6
    } else {
        -x_old / z_sq_old
    };

    // New series admittance.
    let x_new = tcsc.x_orig - x_comp_new;
    let z_sq_new = r * r + x_new * x_new;
    let g_new = if z_sq_new < 1e-12 { 1e6 } else { r / z_sq_new };
    let b_new = if z_sq_new < 1e-12 {
        -1e6
    } else {
        -x_new / z_sq_new
    };

    let dg = g_new - g_old;
    let db = b_new - b_old;

    let f = tcsc.from_idx;
    let t = tcsc.to_idx;

    // Y_ff += (dg + j·db) / tap²
    let dg_ff = dg / tap_sq;
    let db_ff = db / tap_sq;

    // Y_tt += dg + j·db
    let dg_tt = dg;
    let db_tt = db;

    // Y_ft += -(dg + j·db) / (tap × e^(jφ))
    //       = -(dg cosφ + db sinφ, db cosφ − dg sinφ) / tap
    let dg_ft = -(dg * cos_s + db * sin_s) / tap;
    let db_ft = -(db * cos_s - dg * sin_s) / tap;

    // Y_tf += -(dg + j·db) / (tap × e^(−jφ))
    let dg_tf = -(dg * cos_s - db * sin_s) / tap;
    let db_tf = -(db * cos_s + dg * sin_s) / tap;

    [
        (f, f, dg_ff, db_ff),
        (t, t, dg_tt, db_tt),
        (f, t, dg_ft, db_ft),
        (t, f, dg_tf, db_tf),
    ]
}

/// Compute Y-bus deltas for an incremental TCSC reactance change.
///
/// Convenience wrapper: changes from current `x_comp` to `x_comp + delta_x`.
fn tcsc_ybus_delta(tcsc: &TcscState, delta_x: f64) -> [(usize, usize, f64, f64); 4] {
    tcsc_ybus_delta_from_base(tcsc, tcsc.x_comp, tcsc.x_comp + delta_x)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use surge_network::Network;
    use surge_network::network::{Branch, Generator, Load};
    use surge_network::network::{Bus, BusType};
    use surge_network::network::{FactsDevice, FactsMode};

    fn make_3bus_svc() -> Network {
        let mut net = Network::new("svc_test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.loads.push(Load::new(2, 100.0, 50.0)); // 100 MW load at bus 2
        net.loads.push(Load::new(3, 80.0, 40.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.02, 0.15, 0.02));
        net.branches.push(Branch::new_line(1, 3, 0.015, 0.12, 0.02));
        net.generators.push(Generator::new(1, 200.0, 1.05));
        // SVC at bus 2 regulating voltage to 1.02 p.u.
        net.facts_devices.push(FactsDevice {
            name: "SVC1".into(),
            bus_from: 2,
            bus_to: 0,
            mode: FactsMode::ShuntOnly,
            p_setpoint_mw: 0.0,
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.02,
            q_max: 100.0,
            series_reactance_pu: 0.0,
            in_service: true,
            ..FactsDevice::default()
        });
        net
    }

    fn make_3bus_tcsc() -> Network {
        let mut net = Network::new("tcsc_test");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.loads.push(Load::new(2, 80.0, 30.0));
        net.loads.push(Load::new(3, 60.0, 20.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.02, 0.2, 0.02));
        net.branches.push(Branch::new_line(1, 3, 0.015, 0.12, 0.02));
        net.generators.push(Generator::new(1, 160.0, 1.0));
        // TCSC on branch 2→3, desired P = 0.3 p.u.
        net.facts_devices.push(FactsDevice {
            name: "TCSC1".into(),
            bus_from: 2,
            bus_to: 3,
            mode: FactsMode::SeriesOnly,
            p_setpoint_mw: 30.0, // 30 MW
            q_setpoint_mvar: 0.0,
            voltage_setpoint_pu: 1.0,
            q_max: 0.0,
            series_reactance_pu: 0.05,
            in_service: true,
            ..FactsDevice::default()
        });
        net
    }

    #[test]
    fn test_facts_states_init_svc() {
        let net = make_3bus_svc();
        let bus_map = net.bus_index_map();
        let states = FactsStates::new(&net, &bus_map);
        assert_eq!(states.svcs.len(), 1);
        assert_eq!(states.tcscs.len(), 0);
        assert_eq!(states.svcs[0].bus_idx, 1); // bus 2 → index 1
        assert!((states.svcs[0].voltage_setpoint_pu - 1.02).abs() < 1e-10);
        assert!((states.svcs[0].b_max - 1.0).abs() < 1e-10); // 100 MVAr / 100 MVA
    }

    #[test]
    fn test_facts_states_init_tcsc() {
        let net = make_3bus_tcsc();
        let bus_map = net.bus_index_map();
        let states = FactsStates::new(&net, &bus_map);
        assert_eq!(states.tcscs.len(), 1);
        assert_eq!(states.svcs.len(), 0);
        let tcsc = &states.tcscs[0];
        assert!((tcsc.x_comp - 0.05).abs() < 1e-10);
        assert!((tcsc.p_setpoint_mw - 0.3).abs() < 1e-10);
    }

    #[test]
    fn test_svc_outer_loop_solve() {
        // End-to-end test: solve NR with SVC augmentation.
        let net = make_3bus_svc();
        let opts = crate::solver::newton_raphson::AcPfOptions {
            flat_start: true,
            enforce_q_limits: false,
            detect_islands: false,
            ..Default::default()
        };
        let sol = crate::solver::newton_raphson::solve_ac_pf_kernel(&net, &opts).unwrap();
        // SVC should regulate bus 2 voltage close to 1.02 p.u.
        let bus_map = net.bus_index_map();
        let bus2_idx = bus_map[&2];
        assert!(
            (sol.voltage_magnitude_pu[bus2_idx] - 1.02).abs() < 0.005,
            "SVC voltage regulation failed: V_bus2 = {}, expected ~1.02",
            sol.voltage_magnitude_pu[bus2_idx]
        );
    }

    #[test]
    fn test_tcsc_outer_loop_solve() {
        let net = make_3bus_tcsc();
        let opts = crate::solver::newton_raphson::AcPfOptions {
            flat_start: true,
            enforce_q_limits: false,
            detect_islands: false,
            ..Default::default()
        };
        let sol = crate::solver::newton_raphson::solve_ac_pf_kernel(&net, &opts).unwrap();
        assert!(
            sol.iterations < 30,
            "TCSC solve took too many iterations: {}",
            sol.iterations
        );

        // Verify voltages are reasonable (converged to a physical solution).
        let bus_map = net.bus_index_map();
        for &idx in bus_map.values() {
            assert!(
                sol.voltage_magnitude_pu[idx] > 0.8 && sol.voltage_magnitude_pu[idx] < 1.2,
                "Bus voltage out of range: V[{idx}] = {}",
                sol.voltage_magnitude_pu[idx]
            );
        }
    }

    #[test]
    fn test_svc_limit_enforcement() {
        let mut net = make_3bus_svc();
        // Set very low Q limit so SVC hits its limit.
        net.facts_devices[0].q_max = 5.0; // Only 5 MVAr
        let opts = crate::solver::newton_raphson::AcPfOptions {
            flat_start: true,
            enforce_q_limits: false,
            detect_islands: false,
            ..Default::default()
        };
        let sol = crate::solver::newton_raphson::solve_ac_pf_kernel(&net, &opts).unwrap();
        // With only 5 MVAr, SVC can't fully regulate voltage.
        // Solution should still converge.
        let bus_map = net.bus_index_map();
        let bus2_idx = bus_map[&2];
        assert!(
            sol.voltage_magnitude_pu[bus2_idx] < 1.02,
            "SVC at limit should not achieve V_set: V = {}",
            sol.voltage_magnitude_pu[bus2_idx]
        );
    }

    #[test]
    fn test_no_facts_devices_is_noop() {
        let mut net = make_3bus_svc();
        net.facts_devices.clear();
        let bus_map = net.bus_index_map();
        let states = FactsStates::new(&net, &bus_map);
        assert!(states.is_empty());
    }

    #[test]
    fn test_tcsc_ybus_delta_symmetry() {
        // The Y-bus delta for a simple line should be symmetric in G
        // (f,t and t,f get the same ΔG, opposite ΔB sign for shift=0).
        let tcsc = TcscState {
            from_idx: 0,
            to_idx: 1,
            x_orig: 0.2,
            r: 0.02,
            x_comp: 0.0,
            p_setpoint_mw: 0.0,
            x_comp_max: 0.16,
            x_comp_min: -0.1,
            tap: 1.0,
            shift_rad: 0.0,
            at_limit: false,
        };
        let deltas = tcsc_ybus_delta(&tcsc, 0.05);
        // For tap=1, shift=0: dg_ft == dg_tf and db_ft == db_tf
        let (_, _, dg_ft, db_ft) = deltas[2];
        let (_, _, dg_tf, db_tf) = deltas[3];
        assert!(
            (dg_ft - dg_tf).abs() < 1e-12,
            "G deltas should be equal for symmetric line"
        );
        assert!(
            (db_ft - db_tf).abs() < 1e-12,
            "B deltas should be equal for symmetric line"
        );
    }
}
