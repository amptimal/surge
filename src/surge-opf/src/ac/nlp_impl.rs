// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! NlpProblem trait implementation for AC-OPF.
//!
//! This file contains the core NLP evaluation engine: objective, gradient,
//! constraints, Jacobian, and Hessian evaluation callbacks invoked by Ipopt.
#![allow(clippy::needless_range_loop)]

use surge_ac::matrix::mismatch::compute_power_injection;
use surge_network::network::{StorageDispatchMode, StorageParams};

use super::hvdc::{HvdcAcControlMode, HvdcDcControlMode};
use super::problem::{AcOpfProblem, HESS_SKIP};
use super::types::{branch_flow_from, branch_flow_to};
use crate::nlp::NlpProblem;

/// Discharge-side foldback: 0 MW at ``soc_min``, rising linearly to
/// ``p_max`` at the foldback threshold, flat above. ``None`` = no cut.
fn foldback_discharge_cap_nlp(sto: &StorageParams, soc_mwh: f64, p_max: f64) -> f64 {
    match sto.discharge_foldback_soc_mwh {
        None => p_max,
        Some(threshold) => {
            let range = (threshold - sto.soc_min_mwh).max(1e-12);
            let frac = ((soc_mwh - sto.soc_min_mwh) / range).clamp(0.0, 1.0);
            p_max * frac
        }
    }
}

/// Charge-side foldback: 0 MW at ``soc_max``, rising linearly to
/// ``p_max`` at the threshold, flat below. ``None`` = no cut.
fn foldback_charge_cap_nlp(sto: &StorageParams, soc_mwh: f64, p_max: f64) -> f64 {
    match sto.charge_foldback_soc_mwh {
        None => p_max,
        Some(threshold) => {
            let range = (sto.soc_max_mwh - threshold).max(1e-12);
            let frac = ((sto.soc_max_mwh - soc_mwh) / range).clamp(0.0, 1.0);
            p_max * frac
        }
    }
}

impl NlpProblem for AcOpfProblem<'_> {
    fn n_vars(&self) -> usize {
        self.mapping.n_var
    }

    fn n_constraints(&self) -> usize {
        self.mapping.n_con + self.cuts.len()
    }

    fn var_bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let m = &self.mapping;
        let mut lb = vec![0.0; m.n_var];
        let mut ub = vec![0.0; m.n_var];

        // Va bounds: [-π, π] for non-slack buses.
        //
        // S-01 convention note: Va[slack] = 0 is the angle reference, not a
        // physical constraint. The slack bus has no Va variable in the NLP — it
        // is fixed implicitly by absence, so unpack() always returns 0 for it.
        // The DC warm-start (build_initial_point) also sets θ_slack = 0 as
        // the reference. ±π bounds are the MATPOWER convention for non-slack
        // buses: they are effectively unconstrained (no physical network has
        // bus angle differences near ±π in a stable operating point).
        for i in 0..m.n_bus {
            if let Some(idx) = m.va_var(i) {
                lb[idx] = -std::f64::consts::PI;
                ub[idx] = std::f64::consts::PI;
            }
        }

        // Vm bounds: [vmin, vmax], except regulated buses whose voltage target
        // is fixed by an in-service voltage-controlling device. When
        // `enforce_regulated_bus_vm_targets` is `false`, the regulator setpoint
        // becomes a soft target only and Vm at regulated buses keeps its
        // normal `[vmin, vmax]` box bounds (used by market-style formulations
        // that prefer the optimizer to choose voltage rather than honor an
        // exogenous setpoint).
        for i in 0..m.n_bus {
            let idx = m.vm_var(i);
            let regulated_target = self.regulated_bus_vm_targets[i];
            if self.enforce_regulated_bus_vm_targets {
                if let Some(target_vm) = regulated_target {
                    lb[idx] = target_vm;
                    ub[idx] = target_vm;
                    continue;
                }
            }
            if m.has_voltage_slacks() {
                // Widen Vm bounds to allow slack variables room.
                // The original bounds are enforced via penalty constraints.
                lb[idx] = (self.vm_min_orig_pu[i] - 0.5).max(0.0);
                ub[idx] = self.vm_max_orig_pu[i] + 0.5;
            } else {
                lb[idx] = self.network.buses[i].voltage_min_pu;
                ub[idx] = self.network.buses[i].voltage_max_pu;
            }
        }

        // Voltage-magnitude slack variable bounds: [0, 1.0] per bus.
        if m.has_voltage_slacks() {
            for i in 0..m.n_bus {
                lb[m.vm_slack_high_var(i)] = 0.0;
                ub[m.vm_slack_high_var(i)] = 1.0;
                lb[m.vm_slack_low_var(i)] = 0.0;
                ub[m.vm_slack_low_var(i)] = 1.0;
            }
        }

        // Angle-difference slack variable bounds: [0, 2π] per constrained branch.
        if m.has_angle_slacks() {
            use std::f64::consts::PI;
            for ai in 0..m.n_angle_slack {
                lb[m.angle_slack_high_var(ai)] = 0.0;
                ub[m.angle_slack_high_var(ai)] = 2.0 * PI;
                lb[m.angle_slack_low_var(ai)] = 0.0;
                ub[m.angle_slack_low_var(ai)] = 2.0 * PI;
            }
        }

        // Pg bounds: [pmin/base, pmax/base]
        //
        // Native-storage real power is modeled through the dedicated discharge/
        // charge variables, not through generator Pg. Storage generators keep
        // their reactive-power variables, but their Pg is fixed to zero here to
        // avoid double-counting active injection.
        for j in 0..m.n_gen {
            let gi = m.gen_indices[j];
            let g = &self.network.generators[gi];
            let idx = m.pg_var(j);
            if g.is_storage() {
                lb[idx] = 0.0;
                ub[idx] = 0.0;
            } else {
                lb[idx] = g.pmin / self.base_mva;
                ub[idx] = g.pmax / self.base_mva;
            }
        }

        // Qg bounds: [qmin/base, qmax/base]
        for j in 0..m.n_gen {
            let gi = m.gen_indices[j];
            let g = &self.network.generators[gi];
            let idx = m.qg_var(j);
            // Handle case where qmin/qmax might be f64::MIN/MAX
            let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
            let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
            lb[idx] = qmin / self.base_mva;
            ub[idx] = qmax / self.base_mva;
        }

        // Tap ratio bounds: [tap_min, tap_max] for each τ variable.
        for (k, &(_, tau_min, tau_max)) in m.tap_ctrl_branches.iter().enumerate() {
            let idx = m.tap_var(k);
            lb[idx] = tau_min;
            ub[idx] = tau_max;
        }

        // Phase shift bounds: [phase_min_rad, phase_max_rad] for each θ_s variable.
        for (k, &(_, ps_min_rad, ps_max_rad)) in m.ps_ctrl_branches.iter().enumerate() {
            let idx = m.ps_var(k);
            lb[idx] = ps_min_rad;
            ub[idx] = ps_max_rad;
        }

        // Switched shunt bounds: [b_min_pu, b_max_pu] for each b_sw variable.
        for i in 0..m.n_sw {
            let shunt = &self.network.controls.switched_shunts_opf[i];
            let idx = m.sw_var(i);
            lb[idx] = shunt.b_min_pu;
            ub[idx] = shunt.b_max_pu;
        }

        // SVC susceptance bounds.
        for i in 0..m.n_svc {
            lb[m.svc_var(i)] = m.svc_devices[i].b_min;
            ub[m.svc_var(i)] = m.svc_devices[i].b_max;
        }

        // TCSC compensation bounds.
        for i in 0..m.n_tcsc {
            lb[m.tcsc_var(i)] = m.tcsc_devices[i].x_comp_min;
            ub[m.tcsc_var(i)] = m.tcsc_devices[i].x_comp_max;
        }

        // HVDC converter and DC bus voltage bounds.
        if let Some(ref hvdc) = self.hvdc {
            for k in 0..m.n_conv {
                let c = &hvdc.converters[k];
                lb[m.pconv_var(k)] = c.p_min_pu;
                ub[m.pconv_var(k)] = c.p_max_pu;
                lb[m.qconv_var(k)] = c.q_min_pu;
                ub[m.qconv_var(k)] = c.q_max_pu;
                lb[m.iconv_var(k)] = 0.0;
                ub[m.iconv_var(k)] = if c.i_max_pu.is_finite() && c.i_max_pu > 0.0 {
                    c.i_max_pu
                } else {
                    let s_max = (c.p_max_pu.abs().max(c.p_min_pu.abs()).powi(2)
                        + c.q_max_pu.abs().max(c.q_min_pu.abs()).powi(2))
                    .sqrt();
                    let vm_min = {
                        let bv = self.network.buses[c.ac_bus_idx].voltage_min_pu;
                        if bv.is_finite() && bv > 0.0 { bv } else { 0.9 }
                    };
                    s_max / vm_min
                };
            }
            for d in 0..m.n_dc_bus {
                let (vmin, vmax) = hvdc.vdc_bounds[d];
                lb[m.vdc_var(d)] = vmin;
                ub[m.vdc_var(d)] = vmax;
            }
        }

        // Storage discharge/charge bounds.
        //
        // Two constraints per unit s:
        //   dis[s] ∈ [0, min(discharge_mw_max, soc_dis_limit) / base]
        //   ch[s]  ∈ [0, min(charge_mw_max, soc_ch_limit)  / base]
        //
        // dt_hours is the interval duration (set from AcOpfOptions.dt_hours, default 1.0).
        let dt = self.dt_hours;
        for s in 0..m.n_sto {
            let gi = self.storage_gen_indices[s];
            let g = &self.network.generators[gi];
            let sto = g
                .storage
                .as_ref()
                .expect("storage_gen_indices only contains generators with storage");
            let soc = self.storage_soc_mwh[s];
            let eta_ch = sto.charge_efficiency.max(1e-9);
            let eta_dis = sto.discharge_efficiency.max(1e-9);
            // Apply SoC-dependent power foldback first, then cap by the
            // energy available within the interval.
            let p_dis_max = foldback_discharge_cap_nlp(sto, soc, g.discharge_mw_max());
            let p_ch_max = foldback_charge_cap_nlp(sto, soc, g.charge_mw_max());
            // max_dis (pu): energy available to discharge within dt hours —
            // discharging net MW draws 1/η_dis MWh per MW-hr from SoC.
            let soc_dis_limit = ((soc - sto.soc_min_mwh) * eta_dis / dt).max(0.0);
            let dis_ub = p_dis_max.min(soc_dis_limit) / self.base_mva;
            lb[m.discharge_var(s)] = 0.0;
            ub[m.discharge_var(s)] = dis_ub;
            // max_ch (pu): headroom to charge within dt hours — charging net
            // MW stores η_ch MWh per MW-hr into SoC.
            let soc_ch_limit = ((sto.soc_max_mwh - soc) / (dt * eta_ch)).max(0.0);
            let ch_ub = p_ch_max.min(soc_ch_limit) / self.base_mva;
            lb[m.charge_var(s)] = 0.0;
            ub[m.charge_var(s)] = ch_ub;
        }

        for (k, &dl_idx) in self.dispatchable_load_indices.iter().enumerate() {
            let dl = &self.network.market_data.dispatchable_loads[dl_idx];
            lb[m.dl_var(k)] = dl.p_min_pu;
            ub[m.dl_var(k)] = dl.p_max_pu;
            lb[m.dl_q_var(k)] = dl.q_min_pu;
            ub[m.dl_q_var(k)] = dl.q_max_pu;
        }

        if m.has_thermal_limit_slacks() {
            for (ci, ba) in self.branch_admittances.iter().enumerate() {
                let slack_ub = (4.0 * ba.s_max_pu()).max(10.0);
                lb[m.thermal_slack_from_var(ci)] = 0.0;
                ub[m.thermal_slack_from_var(ci)] = slack_ub;
                lb[m.thermal_slack_to_var(ci)] = 0.0;
                ub[m.thermal_slack_to_var(ci)] = slack_ub;
            }
        }

        if m.has_p_bus_balance_slacks() {
            for i in 0..m.n_bus {
                lb[m.p_balance_slack_pos_var(i)] = 0.0;
                ub[m.p_balance_slack_pos_var(i)] = 10.0;
                lb[m.p_balance_slack_neg_var(i)] = 0.0;
                ub[m.p_balance_slack_neg_var(i)] = 10.0;
            }
        }
        if m.has_q_bus_balance_slacks() {
            for i in 0..m.n_bus {
                lb[m.q_balance_slack_pos_var(i)] = 0.0;
                ub[m.q_balance_slack_pos_var(i)] = 10.0;
                lb[m.q_balance_slack_neg_var(i)] = 0.0;
                ub[m.q_balance_slack_neg_var(i)] = 10.0;
            }
        }

        // Reactive reserve variable bounds.
        //   Producers:
        //     lb = 0, ub = (qmax − qmin)/base, or 0 for pqe devices.
        //   Consumers: same rule.
        //   Zone shortfall slacks: [0, +inf].
        //
        // The ub values come from the reactive reserve plan built
        // during `AcOpfProblem::new`, which already collapsed pqe
        // devices to 0 per eqs (117)-(118) / (127)-(128).
        for j in 0..m.n_producer_q_reserve {
            lb[m.producer_q_reserve_up_var(j)] = 0.0;
            ub[m.producer_q_reserve_up_var(j)] =
                self.reactive_reserve_plan.producer_q_reserve_up_ub_pu[j];
            lb[m.producer_q_reserve_down_var(j)] = 0.0;
            ub[m.producer_q_reserve_down_var(j)] =
                self.reactive_reserve_plan.producer_q_reserve_down_ub_pu[j];
        }
        for k in 0..m.n_consumer_q_reserve {
            lb[m.consumer_q_reserve_up_var(k)] = 0.0;
            ub[m.consumer_q_reserve_up_var(k)] =
                self.reactive_reserve_plan.consumer_q_reserve_up_ub_pu[k];
            lb[m.consumer_q_reserve_down_var(k)] = 0.0;
            ub[m.consumer_q_reserve_down_var(k)] =
                self.reactive_reserve_plan.consumer_q_reserve_down_ub_pu[k];
        }
        for i in 0..m.n_zone_q_reserve_up_shortfall {
            lb[m.zone_q_reserve_up_shortfall_var(i)] = 0.0;
            ub[m.zone_q_reserve_up_shortfall_var(i)] = f64::INFINITY;
        }
        for i in 0..m.n_zone_q_reserve_down_shortfall {
            lb[m.zone_q_reserve_down_shortfall_var(i)] = 0.0;
            ub[m.zone_q_reserve_down_shortfall_var(i)] = f64::INFINITY;
        }

        // HVDC point-to-point P variable bounds (pu). One variable per
        // in-service link with `p_dc_min_mw < p_dc_max_mw`, sourced from
        // `HvdcP2PNlpData` and written at the tail of the variable vector.
        if let Some(p2p) = self.hvdc_p2p.as_ref() {
            for (k, link) in p2p.links.iter().enumerate() {
                let idx = m.hvdc_p2p_var(k);
                lb[idx] = link.p_min_pu;
                ub[idx] = link.p_max_pu;
            }
        }

        (lb, ub)
    }

    fn constraint_bounds(&self) -> (Vec<f64>, Vec<f64>) {
        let m = &self.mapping;
        let total_con = m.n_con + self.cuts.len();
        let mut gl = vec![0.0; total_con];
        let mut gu = vec![0.0; total_con];

        // P-balance: g(x) = 0  (rows 0..n_bus)
        // Q-balance: g(x) = 0  (rows n_bus..2*n_bus)
        // Already set to 0/0 (equality)

        // Branch flow (from): -∞ <= Pf² + Qf² <= s_max² (rows 2*n_bus..)
        let n_br = self.branch_admittances.len();
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let row = 2 * m.n_bus + ci;
            gl[row] = f64::NEG_INFINITY;
            gu[row] = ba.s_max_sq;
        }
        // Branch flow (to): -∞ <= Pt² + Qt² <= s_max² (rows 2*n_bus + n_br..)
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let row = 2 * m.n_bus + n_br + ci;
            gl[row] = f64::NEG_INFINITY;
            gu[row] = ba.s_max_sq;
        }

        // Angle difference constraints: angmin_k <= Va_from - Va_to <= angmax_k
        // (rows 2*n_bus + 2*n_br..)
        // angmin/angmax are in radians; ±∞ means unbounded on that side.
        let ang_row_offset = 2 * m.n_bus + 2 * n_br;
        for (ai, &(_, lo, hi)) in m.angle_constrained_branches.iter().enumerate() {
            let row = ang_row_offset + ai;
            gl[row] = if lo.is_finite() {
                lo
            } else {
                f64::NEG_INFINITY
            };
            gu[row] = if hi.is_finite() { hi } else { f64::INFINITY };
        }

        // DC KCL equality constraints: Σ (P_conv_k - loss) + Σ G_dc*V_dc = 0
        // (rows after angle constraints)
        // Already initialized to 0/0 (equality).

        // Converter current-definition equality constraints: P²+Q²-Vm²·I²=0
        // (rows after DC KCL)
        // Already initialized to 0/0 (equality).

        // D-curve / linear-link / flat-headroom constraints:
        //   lhs_lb ≤ q_dev − slope·p_dev + sign·q_reserve ≤ lhs_ub
        for (ci, c) in self.pq_constraints.iter().enumerate() {
            let row = m.pq_con_offset + ci;
            gl[row] = c.lhs_lb;
            gu[row] = c.lhs_ub;
        }
        // Zonal q-reserve balance rows:
        //   requirement_pu ≤ Σ q_reserve + q_shortfall ≤ +∞
        for (i, zone_row) in self.reactive_reserve_plan.zone_rows.iter().enumerate() {
            let row = m.zone_q_reserve_balance_row(i);
            gl[row] = zone_row.requirement_pu;
            gu[row] = f64::INFINITY;
        }
        for row_opt in &m.dispatchable_load_pf_rows {
            if let Some(row) = *row_opt {
                gl[row] = 0.0;
                gu[row] = 0.0;
            }
        }

        // Flowgate constraints: -reverse_limit/base ≤ FG_flow ≤ +limit_mw/base
        let base = self.base_mva;
        for (fi, &fgi) in m.flowgate_indices.iter().enumerate() {
            let fg = &self.network.flowgates[fgi];
            let row = m.fg_con_offset + fi;
            let rev = fg.effective_reverse_or_forward(0);
            gl[row] = -rev / base;
            gu[row] = fg.limit_mw / base;
        }

        // Interface constraints: -limit_reverse_mw/base ≤ flow ≤ +limit_forward_mw/base
        for (ii, &ifi) in m.interface_indices.iter().enumerate() {
            let iface = &self.network.interfaces[ifi];
            let row = m.iface_con_offset + ii;
            gl[row] = -iface.limit_reverse_mw / base;
            gu[row] = iface.limit_forward_mw / base;
        }

        // Voltage-magnitude slack constraints:
        //   High: vm[i] - σ_high[i] ≤ vm_max[i]  →  -∞ ≤ g ≤ vm_max
        //   Low:  -vm[i] - σ_low[i] ≤ -vm_min[i]  →  -∞ ≤ g ≤ -vm_min
        if m.has_voltage_slacks() {
            for i in 0..m.n_vm_slack {
                let high_row = m.vm_slack_con_offset + i;
                gl[high_row] = f64::NEG_INFINITY;
                gu[high_row] = self.vm_max_orig_pu[i];
                let low_row = m.vm_slack_con_offset + m.n_vm_slack + i;
                gl[low_row] = f64::NEG_INFINITY;
                gu[low_row] = -self.vm_min_orig_pu[i];
            }
        }

        // Benders cut inequality constraints: −∞ ≤ α^T Pg ≤ rhs
        for (c, cut) in self.cuts.iter().enumerate() {
            let row = m.n_con + c;
            gl[row] = f64::NEG_INFINITY;
            gu[row] = cut.rhs;
        }

        (gl, gu)
    }

    fn initial_point(&self) -> Vec<f64> {
        self.x0.clone()
    }

    fn eval_objective(&self, x: &[f64]) -> f64 {
        let m = &self.mapping;
        let pg = &x[m.pg_offset..m.pg_offset + m.n_gen];
        let dt = self.dt_hours;
        let mut cost = 0.0;

        // Generator production cost.
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &self.network.generators[gi];
            let p_mw = pg[j] * self.base_mva;
            cost += dt
                * g.cost
                    .as_ref()
                    .expect("generator cost validated in AcOpfProblem::new")
                    .evaluate(p_mw);
            if let Some(target_p_mw) = self.generator_target_tracking_mw[j] {
                // One-sided asymmetric quadratic:
                //   α_up * max(0, Pg − target)² + α_down * max(0, target − Pg)²
                // Reduces to the legacy symmetric `α * (Pg − target)²`
                // when `α_up == α_down`.
                let pair = self.generator_target_tracking_coefficients[j];
                if !pair.is_zero() {
                    let delta_mw = p_mw - target_p_mw;
                    if delta_mw > 0.0 && pair.upward_per_mw2 > 0.0 {
                        cost += dt * pair.upward_per_mw2 * delta_mw * delta_mw;
                    } else if delta_mw < 0.0 && pair.downward_per_mw2 > 0.0 {
                        cost += dt * pair.downward_per_mw2 * delta_mw * delta_mw;
                    }
                }
            }
        }

        // Storage dispatch cost / value for native storage units.
        for s in 0..m.n_sto {
            let gi = self.storage_gen_indices[s];
            let sto = self.network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_indices only contains generators with storage");
            let discharge_mw = x[m.discharge_var(s)] * self.base_mva;
            let charge_mw = x[m.charge_var(s)] * self.base_mva;
            match sto.dispatch_mode {
                StorageDispatchMode::CostMinimization => {
                    let dis_cost = sto.variable_cost_per_mwh + sto.degradation_cost_per_mwh;
                    cost += dt * dis_cost * discharge_mw;
                    cost += dt * sto.degradation_cost_per_mwh * charge_mw;
                }
                StorageDispatchMode::OfferCurve => {
                    if let Some(points) = sto.discharge_offer.as_deref() {
                        cost += dt * StorageParams::market_curve_value(points, discharge_mw);
                    }
                    if let Some(points) = sto.charge_bid.as_deref() {
                        cost -= dt * StorageParams::market_curve_value(points, charge_mw);
                    }
                }
                StorageDispatchMode::SelfSchedule => {}
            }
        }
        for (k, &dl_idx) in self.dispatchable_load_indices.iter().enumerate() {
            let dl = &self.network.market_data.dispatchable_loads[dl_idx];
            cost += dt
                * dl.cost_model.objective_contribution(
                    x[m.dl_var(k)],
                    dl.p_sched_pu,
                    self.base_mva,
                );
            if let Some(target_p_mw) = self.dispatchable_load_target_tracking_mw[k] {
                let pair = self.dispatchable_load_target_tracking_coefficients[k];
                if !pair.is_zero() {
                    let p_served_mw = x[m.dl_var(k)] * self.base_mva;
                    let delta_mw = p_served_mw - target_p_mw;
                    if delta_mw > 0.0 && pair.upward_per_mw2 > 0.0 {
                        cost += dt * pair.upward_per_mw2 * delta_mw * delta_mw;
                    } else if delta_mw < 0.0 && pair.downward_per_mw2 > 0.0 {
                        cost += dt * pair.downward_per_mw2 * delta_mw * delta_mw;
                    }
                }
            }
        }
        if m.has_thermal_limit_slacks() && self.thermal_limit_slack_penalty_per_mva > 0.0 {
            let penalty = dt * self.thermal_limit_slack_penalty_per_mva * self.base_mva;
            for ci in 0..self.branch_admittances.len() {
                cost += penalty * x[m.thermal_slack_from_var(ci)];
                cost += penalty * x[m.thermal_slack_to_var(ci)];
            }
        }
        if m.has_p_bus_balance_slacks() {
            let p_penalty = dt * self.bus_active_power_balance_slack_penalty_per_mw * self.base_mva;
            for i in 0..m.n_bus {
                cost += p_penalty * x[m.p_balance_slack_pos_var(i)];
                cost += p_penalty * x[m.p_balance_slack_neg_var(i)];
            }
        }
        if m.has_q_bus_balance_slacks() {
            let q_penalty =
                dt * self.bus_reactive_power_balance_slack_penalty_per_mvar * self.base_mva;
            for i in 0..m.n_bus {
                cost += q_penalty * x[m.q_balance_slack_pos_var(i)];
                cost += q_penalty * x[m.q_balance_slack_neg_var(i)];
            }
        }
        if m.has_voltage_slacks() {
            let vm_penalty = dt * self.voltage_magnitude_slack_penalty_per_pu * self.base_mva;
            for i in 0..m.n_vm_slack {
                cost += vm_penalty * x[m.vm_slack_high_var(i)];
                cost += vm_penalty * x[m.vm_slack_low_var(i)];
            }
        }
        if m.has_angle_slacks() {
            let ang_penalty = dt * self.angle_difference_slack_penalty_per_rad * self.base_mva;
            for ai in 0..m.n_angle_slack {
                cost += ang_penalty * x[m.angle_slack_high_var(ai)];
                cost += ang_penalty * x[m.angle_slack_low_var(ai)];
            }
        }

        // Per-device reactive-reserve cost. Costs are stored in
        // `$/pu-hr` on the plan so the product with `x[var]` (in pu)
        // is already in `$/hr`.
        for j in 0..m.n_producer_q_reserve {
            let qru = x[m.producer_q_reserve_up_var(j)];
            let qrd = x[m.producer_q_reserve_down_var(j)];
            cost += self
                .reactive_reserve_plan
                .producer_q_reserve_up_cost_per_pu_hr[j]
                * qru
                * dt;
            cost += self
                .reactive_reserve_plan
                .producer_q_reserve_down_cost_per_pu_hr[j]
                * qrd
                * dt;
        }
        for k in 0..m.n_consumer_q_reserve {
            let qru = x[m.consumer_q_reserve_up_var(k)];
            let qrd = x[m.consumer_q_reserve_down_var(k)];
            cost += self
                .reactive_reserve_plan
                .consumer_q_reserve_up_cost_per_pu_hr[k]
                * qru
                * dt;
            cost += self
                .reactive_reserve_plan
                .consumer_q_reserve_down_cost_per_pu_hr[k]
                * qrd
                * dt;
        }
        // Zonal reactive-reserve shortfall penalty.
        // `shortfall_cost_per_pu_hr` is already scaled to the pu basis
        // of the slack variables.
        for zone_row in &self.reactive_reserve_plan.zone_rows {
            cost += dt * zone_row.shortfall_cost_per_pu_hr * x[zone_row.shortfall_var];
        }
        cost
    }

    fn eval_gradient(&self, x: &[f64], grad: &mut [f64]) {
        let m = &self.mapping;
        let dt = self.dt_hours;
        grad.fill(0.0);

        let pg = &x[m.pg_offset..m.pg_offset + m.n_gen];

        // Generator cost gradient (consistent with eval_objective).
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &self.network.generators[gi];
            let p_mw = pg[j] * self.base_mva;
            // df/dPg_pu = df/dPg_mw * dPg_mw/dPg_pu = marginal_cost * base_mva
            grad[m.pg_var(j)] = g
                .cost
                .as_ref()
                .expect("generator cost validated in AcOpfProblem::new")
                .marginal_cost(p_mw)
                * self.base_mva
                * dt;
            if let Some(target_p_mw) = self.generator_target_tracking_mw[j] {
                let pair = self.generator_target_tracking_coefficients[j];
                if !pair.is_zero() {
                    let delta_mw = p_mw - target_p_mw;
                    // d/d(Pg_pu) of the one-sided quadratic:
                    //   delta > 0 → 2 * α_up * delta * base
                    //   delta < 0 → 2 * α_down * delta * base
                    // At delta = 0 both sides are zero; the Hessian is
                    // discontinuous there but the NLP handles it fine.
                    let penalty = if delta_mw > 0.0 {
                        pair.upward_per_mw2
                    } else if delta_mw < 0.0 {
                        pair.downward_per_mw2
                    } else {
                        0.0
                    };
                    if penalty > 0.0 {
                        grad[m.pg_var(j)] += 2.0 * penalty * delta_mw * self.base_mva * dt;
                    }
                }
            }
        }

        // Storage cost gradients.
        for s in 0..m.n_sto {
            let gi = self.storage_gen_indices[s];
            let sto = self.network.generators[gi]
                .storage
                .as_ref()
                .expect("storage_gen_indices only contains generators with storage");
            let discharge_mw = x[m.discharge_var(s)] * self.base_mva;
            let charge_mw = x[m.charge_var(s)] * self.base_mva;
            match sto.dispatch_mode {
                StorageDispatchMode::CostMinimization => {
                    let dis_cost = sto.variable_cost_per_mwh + sto.degradation_cost_per_mwh;
                    grad[m.discharge_var(s)] = dis_cost * self.base_mva * dt;
                    grad[m.charge_var(s)] = sto.degradation_cost_per_mwh * self.base_mva * dt;
                }
                StorageDispatchMode::OfferCurve => {
                    grad[m.discharge_var(s)] = sto
                        .discharge_offer
                        .as_deref()
                        .map(|points| {
                            StorageParams::market_curve_marginal_value(points, discharge_mw)
                                * self.base_mva
                                * dt
                        })
                        .unwrap_or(0.0);
                    grad[m.charge_var(s)] = sto
                        .charge_bid
                        .as_deref()
                        .map(|points| {
                            -StorageParams::market_curve_marginal_value(points, charge_mw)
                                * self.base_mva
                                * dt
                        })
                        .unwrap_or(0.0);
                }
                StorageDispatchMode::SelfSchedule => {}
            }
        }
        for (k, &dl_idx) in self.dispatchable_load_indices.iter().enumerate() {
            let dl = &self.network.market_data.dispatchable_loads[dl_idx];
            grad[m.dl_var(k)] = dl.cost_model.d_obj_d_p(x[m.dl_var(k)], self.base_mva) * dt;
            if let Some(target_p_mw) = self.dispatchable_load_target_tracking_mw[k] {
                let pair = self.dispatchable_load_target_tracking_coefficients[k];
                if !pair.is_zero() {
                    let p_served_mw = x[m.dl_var(k)] * self.base_mva;
                    let delta_mw = p_served_mw - target_p_mw;
                    let penalty = if delta_mw > 0.0 {
                        pair.upward_per_mw2
                    } else if delta_mw < 0.0 {
                        pair.downward_per_mw2
                    } else {
                        0.0
                    };
                    if penalty > 0.0 {
                        grad[m.dl_var(k)] += 2.0 * penalty * delta_mw * self.base_mva * dt;
                    }
                }
            }
        }
        if m.has_thermal_limit_slacks() && self.thermal_limit_slack_penalty_per_mva > 0.0 {
            let penalty = dt * self.thermal_limit_slack_penalty_per_mva * self.base_mva;
            for ci in 0..self.branch_admittances.len() {
                grad[m.thermal_slack_from_var(ci)] = penalty;
                grad[m.thermal_slack_to_var(ci)] = penalty;
            }
        }
        if m.has_p_bus_balance_slacks() {
            let p_penalty = dt * self.bus_active_power_balance_slack_penalty_per_mw * self.base_mva;
            for i in 0..m.n_bus {
                grad[m.p_balance_slack_pos_var(i)] = p_penalty;
                grad[m.p_balance_slack_neg_var(i)] = p_penalty;
            }
        }
        if m.has_q_bus_balance_slacks() {
            let q_penalty =
                dt * self.bus_reactive_power_balance_slack_penalty_per_mvar * self.base_mva;
            for i in 0..m.n_bus {
                grad[m.q_balance_slack_pos_var(i)] = q_penalty;
                grad[m.q_balance_slack_neg_var(i)] = q_penalty;
            }
        }
        if m.has_voltage_slacks() {
            let vm_penalty = dt * self.voltage_magnitude_slack_penalty_per_pu * self.base_mva;
            for i in 0..m.n_vm_slack {
                grad[m.vm_slack_high_var(i)] = vm_penalty;
                grad[m.vm_slack_low_var(i)] = vm_penalty;
            }
        }
        if m.has_angle_slacks() {
            let ang_penalty = dt * self.angle_difference_slack_penalty_per_rad * self.base_mva;
            for ai in 0..m.n_angle_slack {
                grad[m.angle_slack_high_var(ai)] = ang_penalty;
                grad[m.angle_slack_low_var(ai)] = ang_penalty;
            }
        }
        // Reactive-reserve cost gradient. The objective term is linear
        // in each variable, so the gradient is just the stored cost
        // coefficient.
        for j in 0..m.n_producer_q_reserve {
            grad[m.producer_q_reserve_up_var(j)] = self
                .reactive_reserve_plan
                .producer_q_reserve_up_cost_per_pu_hr[j]
                * dt;
            grad[m.producer_q_reserve_down_var(j)] = self
                .reactive_reserve_plan
                .producer_q_reserve_down_cost_per_pu_hr[j]
                * dt;
        }
        for k in 0..m.n_consumer_q_reserve {
            grad[m.consumer_q_reserve_up_var(k)] = self
                .reactive_reserve_plan
                .consumer_q_reserve_up_cost_per_pu_hr[k]
                * dt;
            grad[m.consumer_q_reserve_down_var(k)] = self
                .reactive_reserve_plan
                .consumer_q_reserve_down_cost_per_pu_hr[k]
                * dt;
        }
        for zone_row in &self.reactive_reserve_plan.zone_rows {
            grad[zone_row.shortfall_var] = zone_row.shortfall_cost_per_pu_hr * dt;
        }
    }

    fn eval_constraints(&self, x: &[f64], g: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, pg, qg) = m.extract_voltages_and_dispatch(x);

        // Compute AC power injections
        let (p_calc, q_calc) = compute_power_injection(&self.ybus, vm, &va);

        // P-balance: P_calc[i] - Σ Pg_at_bus_i + Pd_i/base = 0
        //
        // Note on shunt vs. load (S-08): p_calc[i] already includes the shunt conductance
        // gs*Vm² via the Y-bus diagonal entry (G_ii = gs/tap² + Σ neighbours g_ij). The term
        // pd/base_mva is the fixed constant-power demand and is separate from the
        // voltage-dependent shunt. This follows the MATPOWER convention: pd/qd = fixed loads,
        // gs/bs = shunt admittances. There is no double-count.
        // PSS/E note: ensure load records are mapped to pd/qd, not to gs/bs.
        for i in 0..m.n_bus {
            let mut p_gen = 0.0;
            for &lj in &m.bus_gen_map[i] {
                p_gen += pg[lj];
            }
            g[i] = p_calc[i] - p_gen + self.bus_pd_mw[i] / self.base_mva;
            if m.has_p_bus_balance_slacks() {
                g[i] += x[m.p_balance_slack_pos_var(i)] - x[m.p_balance_slack_neg_var(i)];
            }
        }

        // Storage P-balance correction: each unit injects (dis[s] − ch[s]) at its bus.
        // P-balance at bus i: P_calc[i] − Σ Pg − (dis[s] − ch[s]) + Pd/base = 0
        for s in 0..m.n_sto {
            let bus = m.storage_bus_idx[s];
            g[bus] -= x[m.discharge_var(s)] - x[m.charge_var(s)];
        }
        for k in 0..m.n_dl {
            let bus = m.dispatchable_load_bus_idx[k];
            let p_served = x[m.dl_var(k)];
            g[bus] += p_served;
        }

        // HVDC point-to-point P contributions (split-loss formulation).
        //
        // Sign convention mirrors how loads / storage enter the P balance:
        //   `g[i] = p_calc[i] − Σ Pg_i + Pd_i/base + ...`
        //
        // Split-loss model: half of the quadratic DC-line loss
        // `c_pu * Pg²` is attributed to each terminal. This keeps the
        // Jacobian and Hessian smooth across the `Pg = 0` direction
        // flip (the loss is an even function of Pg), at the modeling
        // cost of splitting the physical loss 50/50 between the two
        // terminals rather than attributing all of it to the receiver.
        //
        // For a lossless link (`c_pu = 0`) this reduces to:
        //   g[from] += Pg,    g[to] -= Pg
        // i.e. the from-terminal withdraws `Pg` (like a load) and the
        // to-terminal injects `Pg` (like a generator).
        //
        // For a lossy link:
        //   g[from] += Pg + 0.5*c*Pg²
        //   g[to]   -= Pg − 0.5*c*Pg²   ==  -Pg + 0.5*c*Pg²
        // The from-bus sees `Pg + loss/2` of withdrawal, the to-bus
        // sees `Pg - loss/2` of net injection, and the total power
        // "disappeared" into heat is `c * Pg²` — covered by the rest
        // of the AC network via generation.
        for k in 0..m.n_hvdc_p2p_links {
            let p_hvdc = x[m.hvdc_p2p_var(k)];
            let c_pu = m.hvdc_p2p_loss_c_pu[k];
            let half_loss = 0.5 * c_pu * p_hvdc * p_hvdc;
            g[m.hvdc_p2p_from_bus_idx[k]] += p_hvdc + half_loss;
            g[m.hvdc_p2p_to_bus_idx[k]] += -p_hvdc + half_loss;
        }

        // Q-balance: Q_calc[i] - Σ Qg_at_bus_i + Qd_i/base = 0
        for i in 0..m.n_bus {
            let mut q_gen = 0.0;
            for &lj in &m.bus_gen_map[i] {
                q_gen += qg[lj];
            }
            g[m.n_bus + i] = q_calc[i] - q_gen + self.bus_qd_mvar[i] / self.base_mva;
            if m.has_q_bus_balance_slacks() {
                g[m.n_bus + i] += x[m.q_balance_slack_pos_var(i)] - x[m.q_balance_slack_neg_var(i)];
            }
        }
        for k in 0..m.n_dl {
            let bus = m.dispatchable_load_bus_idx[k];
            g[m.n_bus + bus] += x[m.dl_q_var(k)];
        }

        // Branch flow limits (from): Pf² + Qf²
        let n_br = self.branch_admittances.len();
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let (pf, qf) = branch_flow_from(ba, vm, &va);
            let mut row_value = pf * pf + qf * qf;
            if m.has_thermal_limit_slacks() {
                let sigma = x[m.thermal_slack_from_var(ci)];
                row_value -= 2.0 * ba.s_max_pu() * sigma + sigma * sigma;
            }
            g[2 * m.n_bus + ci] = row_value;
        }
        // Branch flow limits (to): Pt² + Qt²
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let (pt, qt) = branch_flow_to(ba, vm, &va);
            let mut row_value = pt * pt + qt * qt;
            if m.has_thermal_limit_slacks() {
                let sigma = x[m.thermal_slack_to_var(ci)];
                row_value -= 2.0 * ba.s_max_pu() * sigma + sigma * sigma;
            }
            g[2 * m.n_bus + n_br + ci] = row_value;
        }

        // Angle difference constraints: g_k = Va_from - Va_to
        // Bounds [angmin_k, angmax_k] enforced by Ipopt via constraint_bounds().
        //
        // When angle slacks are active, the residual is relaxed:
        //   g_k = Va_from - Va_to - σ_high[k] + σ_low[k]
        // so the same [angmin, angmax] bounds become effectively
        // `angmin - σ_low ≤ Va_from - Va_to ≤ angmax + σ_high`.
        let ang_row_offset = 2 * m.n_bus + 2 * n_br;
        for (ai, &(br_idx, _, _)) in m.angle_constrained_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let f = self.bus_map[&br.from_bus];
            let t = self.bus_map[&br.to_bus];
            let mut residual = va[f] - va[t];
            if m.has_angle_slacks() {
                residual -= x[m.angle_slack_high_var(ai)];
                residual += x[m.angle_slack_low_var(ai)];
            }
            g[ang_row_offset + ai] = residual;
        }

        // Switched shunt injection correction.
        //
        // The Y-bus was built with the case-data bus.shunt_susceptance_mvar shunt values (no switched shunts
        // in the initial Y-bus; b_sw variables start at 0 in the NLP).  Each switched
        // shunt contributes an additional reactive injection of +b_sw * Vm² (capacitive
        // convention: positive b_sw raises voltage).  We subtract this from the Q-balance
        // residual (Q_calc already includes y-bus shunts; b_sw is additive).
        //
        //   Q-balance:  Q_calc[k] - Σ Qg + Qd/base = 0
        //   With shunt:  Q_calc[k] + b_sw*Vm[k]² - Σ Qg + Qd/base = 0
        //   → correction: g[n_bus + k] -= b_sw * vm[k]²
        if self.optimize_switched_shunts {
            for i in 0..m.n_sw {
                let k = m.switched_shunt_bus_idx[i];
                let b_sw = x[m.sw_var(i)];
                g[m.n_bus + k] -= b_sw * vm[k] * vm[k];
            }
        }

        // SVC reactive injection: Q_svc = b_svc * Vm[k]²
        // Correction to Q-balance: g[n_bus + k] -= b_svc * Vm[k]²
        if self.optimize_svc {
            for i in 0..m.n_svc {
                let k = m.svc_devices[i].bus_idx;
                let b_svc = x[m.svc_var(i)];
                g[m.n_bus + k] -= b_svc * vm[k] * vm[k];
            }
        }

        // TCSC power flow correction: delta(P,Q) from modified branch impedance.
        if self.optimize_tcsc {
            for i in 0..m.n_tcsc {
                let tcsc = &m.tcsc_devices[i];
                let x_comp = x[m.tcsc_var(i)];
                let x_eff = tcsc.x_orig - x_comp;
                let r = tcsc.r;

                // Series admittance with original and modified x
                let z_sq_orig = r * r + tcsc.x_orig * tcsc.x_orig;
                let g_s0 = r / z_sq_orig;
                let b_s0 = -tcsc.x_orig / z_sq_orig;

                let z_sq = r * r + x_eff * x_eff;
                let g_s = r / z_sq;
                let b_s = -x_eff / z_sq;

                // Delta series admittance
                let dg_s = g_s - g_s0;
                let db_s = b_s - b_s0;

                // Pi-circuit delta parameters (including tap and shift)
                let cos_s = tcsc.shift_rad.cos();
                let sin_s = tcsc.shift_rad.sin();
                let tap = tcsc.tap;
                let tap2 = tap * tap;

                let dg_ff = dg_s / tap2;
                let db_ff = db_s / tap2;
                let dg_ft = -(dg_s * cos_s - db_s * sin_s) / tap;
                let db_ft = -(dg_s * sin_s + db_s * cos_s) / tap;
                let dg_tt = dg_s;
                let db_tt = db_s;
                let dg_tf = -(dg_s * cos_s + db_s * sin_s) / tap;
                let db_tf = (dg_s * sin_s - db_s * cos_s) / tap;

                let fi = tcsc.from_idx;
                let ti = tcsc.to_idx;
                let vf = vm[fi];
                let vt = vm[ti];
                let theta_ft = va[fi] - va[ti];
                let cos_ft = theta_ft.cos();
                let sin_ft = theta_ft.sin();

                // Delta P and Q at from-bus
                let dp_f = vf * vf * dg_ff + vf * vt * (dg_ft * cos_ft + db_ft * sin_ft);
                let dq_f = -vf * vf * db_ff + vf * vt * (dg_ft * sin_ft - db_ft * cos_ft);

                // Delta P and Q at to-bus (theta_tf = -theta_ft)
                let dp_t = vt * vt * dg_tt + vt * vf * (dg_tf * cos_ft - db_tf * sin_ft);
                let dq_t = -vt * vt * db_tt - vt * vf * (dg_tf * sin_ft + db_tf * cos_ft);

                // Apply corrections to power balance
                g[fi] += dp_f;
                g[m.n_bus + fi] += dq_f;
                g[ti] += dp_t;
                g[m.n_bus + ti] += dq_t;
            }
        }

        // Tap ratio and phase shift power balance corrections.
        //
        // The Y-bus was built with case-data tap ratios and phase shifts (τ_0, θ_s0).
        // For tap-/phase-controlled branches the NLP variables τ and θ_s may differ
        // from the base values.  We correct the power balance residuals by computing
        // the delta flows:
        //
        //   ΔPf = Pf(τ_var, θ_s_var, Va, Vm) - Pf(τ_0, θ_s0, Va, Vm)
        //
        // and adding those corrections to the from-bus P-balance and the to-bus
        // P-balance (with opposite sign), and similarly for Q.
        //
        // Pi-circuit flows (from-side):
        //   Pf = Vf²·G_ff + Vf·Vt·(G_ft·cos θ + B_ft·sin θ)
        //   Qf = −Vf²·B_ff + Vf·Vt·(G_ft·sin θ − B_ft·cos θ)
        // where θ = Va_f − Va_t + (shift_base − shift_var)  [for phase shift only]
        // and G_ff, B_ff, G_ft, B_ft are functions of τ.
        //
        // Pi-circuit flows (to-side):
        //   Pt = Vt²·G_tt + Vt·Vf·(G_tf·cos θ_tf + B_tf·sin θ_tf)
        //   Qt = −Vt²·B_tt + Vt·Vf·(G_tf·sin θ_tf − B_tf·cos θ_tf)
        // where G_tt, B_tt don't depend on τ (they use the series admittance directly).
        //
        // The correction is applied to both buses to maintain global power balance.

        // Helper: compute pi-circuit admittance params for a branch
        // given variable tap `tau` and phase shift `shift_rad`.
        // Process tap-controllable branches
        for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };

            let shift_rad = br.phase_shift_rad;
            let cos_s = shift_rad.cos();
            let sin_s = shift_rad.sin();

            let vf = vm[fi];
            let vt = vm[ti];
            let theta = va[fi] - va[ti];
            let (sin_t, cos_t) = theta.sin_cos();
            let theta_tf = va[ti] - va[fi];
            let (sin_tf, cos_tf) = theta_tf.sin_cos();

            // Base tap (case-data); MATPOWER convention: tap=0 → 1.0
            let tau_0 = br.effective_tap();
            // Variable tap
            let tau_var = x[m.tap_var(k)];

            // --- Base admittances (tau_0) ---
            let tau0_sq = tau_0 * tau_0;
            let g_ff0 = gs / tau0_sq;
            let b_ff0 = (bs + br.b / 2.0) / tau0_sq;
            let g_ft0 = -(gs * cos_s - bs * sin_s) / tau_0;
            let b_ft0 = -(gs * sin_s + bs * cos_s) / tau_0;
            let g_tt0 = gs; // to-side doesn't depend on tap
            let b_tt0 = bs + br.b / 2.0;
            let g_tf0 = -(gs * cos_s + bs * sin_s) / tau_0;
            let b_tf0 = (gs * sin_s - bs * cos_s) / tau_0;

            // --- Variable admittances (tau_var) ---
            let tau_sq = tau_var * tau_var;
            let g_ff = gs / tau_sq;
            let b_ff = (bs + br.b / 2.0) / tau_sq;
            let g_ft = -(gs * cos_s - bs * sin_s) / tau_var;
            let b_ft = -(gs * sin_s + bs * cos_s) / tau_var;
            // g_tt, b_tt don't change with tap
            let g_tf = -(gs * cos_s + bs * sin_s) / tau_var;
            let b_tf = (gs * sin_s - bs * cos_s) / tau_var;

            // Base flows
            let pf0 = vf * vf * g_ff0 + vf * vt * (g_ft0 * cos_t + b_ft0 * sin_t);
            let qf0 = -vf * vf * b_ff0 + vf * vt * (g_ft0 * sin_t - b_ft0 * cos_t);
            let pt0 = vt * vt * g_tt0 + vt * vf * (g_tf0 * cos_tf + b_tf0 * sin_tf);
            let qt0 = -vt * vt * b_tt0 + vt * vf * (g_tf0 * sin_tf - b_tf0 * cos_tf);

            // Variable flows
            let pf_var = vf * vf * g_ff + vf * vt * (g_ft * cos_t + b_ft * sin_t);
            let qf_var = -vf * vf * b_ff + vf * vt * (g_ft * sin_t - b_ft * cos_t);
            let pt_var = vt * vt * g_tt0 + vt * vf * (g_tf * cos_tf + b_tf * sin_tf);
            let qt_var = -vt * vt * b_tt0 + vt * vf * (g_tf * sin_tf - b_tf * cos_tf);

            // Delta flows = variable - base
            let dpf = pf_var - pf0;
            let dqf = qf_var - qf0;
            let dpt = pt_var - pt0;
            let dqt = qt_var - qt0;

            // Apply to P/Q balance: from-bus gains delta injection, to-bus loses it
            g[fi] += dpf; // P-balance at from-bus
            g[m.n_bus + fi] += dqf; // Q-balance at from-bus
            g[ti] += dpt; // P-balance at to-bus
            g[m.n_bus + ti] += dqt; // Q-balance at to-bus
        }

        // Process phase-shift-controllable branches
        for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };

            let tap = br.effective_tap();
            let tap_sq = tap * tap;

            // Base phase shift (case-data)
            let shift0_rad = br.phase_shift_rad;
            // Variable phase shift
            let shift_var_rad = x[m.ps_var(k)];

            let vf = vm[fi];
            let vt = vm[ti];
            let theta_ft = va[fi] - va[ti];
            let theta_tf = va[ti] - va[fi];

            // Helper: compute pi-circuit flows for a given shift
            let flows_for_shift = |shift_rad: f64| -> (f64, f64, f64, f64) {
                let cos_s = shift_rad.cos();
                let sin_s = shift_rad.sin();
                let g_ff = gs / tap_sq;
                let b_ff = (bs + br.b / 2.0) / tap_sq;
                let g_ft = -(gs * cos_s - bs * sin_s) / tap;
                let b_ft = -(gs * sin_s + bs * cos_s) / tap;
                let g_tt = gs;
                let b_tt = bs + br.b / 2.0;
                let g_tf = -(gs * cos_s + bs * sin_s) / tap;
                let b_tf = (gs * sin_s - bs * cos_s) / tap;

                let (sin_t, cos_t) = theta_ft.sin_cos();
                let (sin_tf, cos_tf) = theta_tf.sin_cos();

                let pf = vf * vf * g_ff + vf * vt * (g_ft * cos_t + b_ft * sin_t);
                let qf = -vf * vf * b_ff + vf * vt * (g_ft * sin_t - b_ft * cos_t);
                let pt = vt * vt * g_tt + vt * vf * (g_tf * cos_tf + b_tf * sin_tf);
                let qt = -vt * vt * b_tt + vt * vf * (g_tf * sin_tf - b_tf * cos_tf);
                (pf, qf, pt, qt)
            };

            let (pf0, qf0, pt0, qt0) = flows_for_shift(shift0_rad);
            let (pf_var, qf_var, pt_var, qt_var) = flows_for_shift(shift_var_rad);

            let dpf = pf_var - pf0;
            let dqf = qf_var - qf0;
            let dpt = pt_var - pt0;
            let dqt = qt_var - qt0;

            g[fi] += dpf;
            g[m.n_bus + fi] += dqf;
            g[ti] += dpt;
            g[m.n_bus + ti] += dqt;
        }

        // --- HVDC converter P/Q corrections and DC KCL constraints ---
        //
        // P_conv[k] withdraws power from the AC bus (converter consumes P from AC side):
        //   P-balance[ac_bus_k]: add +P_conv_k (increases load at that bus)
        // Q_conv[k] similarly:
        //   Q-balance[ac_bus_k]: add +Q_conv_k
        //
        // DC KCL at each DC bus d:
        //   Σ_{k∈d} (P_conv_k - loss_a_pu_k) + Σ_j G_dc(d,j)*V_dc_d*V_dc_j = 0
        if let Some(ref hvdc) = self.hvdc {
            // Add P_conv and Q_conv to AC bus power balance.
            for k in 0..m.n_conv {
                let ac_bus = m.conv_ac_bus[k];
                g[ac_bus] += x[m.pconv_var(k)];
                g[m.n_bus + ac_bus] += x[m.qconv_var(k)];
            }

            // DC KCL constraints with full quadratic loss model.
            for d in 0..m.n_dc_bus {
                let mut dc_kcl = 0.0;
                // Σ_{k∈d} (P_conv_k - loss_a - loss_b·I_conv_k - loss_c·I_conv_k²)
                for &k in &hvdc.dc_bus_conv_map[d] {
                    let c = &hvdc.converters[k];
                    let ic = x[m.iconv_var(k)];
                    dc_kcl +=
                        x[m.pconv_var(k)] - c.loss_a_pu - c.loss_linear * ic - c.loss_c * ic * ic;
                }
                // + Σ_j G_dc(d,j)*V_dc_d*V_dc_j
                let vd = x[m.vdc_var(d)];
                for j in 0..m.n_dc_bus {
                    dc_kcl += hvdc.g_dc[d][j] * vd * x[m.vdc_var(j)];
                }
                g[m.dc_kcl_row_offset + d] = dc_kcl;
            }

            // Converter current-definition equality constraints:
            //   h_k: P_conv_k² + Q_conv_k² - Vm[ac_bus_k]² · I_conv_k² = 0
            for k in 0..m.n_conv {
                let p = x[m.pconv_var(k)];
                let q = x[m.qconv_var(k)];
                let vm = x[m.vm_offset + m.conv_ac_bus[k]];
                let ic = x[m.iconv_var(k)];
                g[m.iconv_eq_row_offset + k] = p * p + q * q - vm * vm * ic * ic;
            }

            // Converter DC-control equations.
            for k in 0..m.n_conv {
                let c = &hvdc.converters[k];
                let ic = x[m.iconv_var(k)];
                g[m.dc_control_row(k)] = match c.dc_control {
                    HvdcDcControlMode::Power => {
                        x[m.pconv_var(k)]
                            - c.loss_a_pu
                            - c.loss_linear * ic
                            - c.loss_c * ic * ic
                            - c.p_dc_set_pu
                    }
                    HvdcDcControlMode::Voltage => {
                        x[m.vdc_var(c.dc_bus_idx)] - c.voltage_dc_setpoint_pu
                    }
                };
            }

            // Converter AC-control equations.
            for k in 0..m.n_conv {
                let c = &hvdc.converters[k];
                g[m.ac_control_row(k)] = match c.ac_control {
                    HvdcAcControlMode::ReactivePower => x[m.qconv_var(k)] - c.q_ac_set_pu,
                    HvdcAcControlMode::AcVoltage => {
                        x[m.vm_var(c.ac_bus_idx)] - c.voltage_ac_setpoint_pu
                    }
                };
            }
        }

        // D-curve / linear-link / q-headroom constraints:
        //   g[row] = q_dev − slope·p_dev + reserve_sign · q_reserve
        //
        // Producers read (Pg, Qg); consumers read the dispatchable-load
        // variables. The optional q-reserve term couples reactive
        // reserves to the p-q curve by adding ±q^qru / q^qrd on the
        // LHS.
        use super::pq_curve::PqDeviceKind;
        for (ci, c) in self.pq_constraints.iter().enumerate() {
            let (p_dev, q_dev) = match c.kind {
                PqDeviceKind::Producer => (pg[c.device_local], qg[c.device_local]),
                PqDeviceKind::Consumer => {
                    (x[m.dl_var(c.device_local)], x[m.dl_q_var(c.device_local)])
                }
            };
            let base = q_dev - c.slope * p_dev;
            let reserve_term = match c.q_reserve_var {
                Some(col) => c.q_reserve_sign * x[col],
                None => 0.0,
            };
            g[m.pq_con_offset + ci] = base + reserve_term;
        }
        for (k, row_opt) in m.dispatchable_load_pf_rows.iter().enumerate() {
            if let Some(row) = *row_opt {
                g[row] = x[m.dl_q_var(k)] - m.dispatchable_load_pf_ratio[k] * x[m.dl_var(k)];
            }
        }

        // Zonal reactive-reserve balance:
        //   g[row] = Σ q_reserve_participant + q_shortfall
        // Row bound `[requirement_pu, +∞]` is set in constraint_bounds().
        for (i, zone_row) in self.reactive_reserve_plan.zone_rows.iter().enumerate() {
            let row = m.zone_q_reserve_balance_row(i);
            let mut sum = 0.0_f64;
            for &col in &zone_row.participant_cols {
                sum += x[col];
            }
            sum += x[zone_row.shortfall_var];
            g[row] = sum;
        }

        // Flowgate constraints: FG_flow = Σ_k coeff_k * P_f_k(V, θ)
        for (fi, fgd) in self.fg_data.iter().enumerate() {
            let mut flow = 0.0;
            for entry in &fgd.branches {
                let ba = &entry.adm;
                let vi = vm[ba.from];
                let vj = vm[ba.to];
                let theta = va[ba.from] - va[ba.to];
                let (sin_t, cos_t) = theta.sin_cos();
                let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                flow += entry.coeff * pf;
            }
            g[m.fg_con_offset + fi] = flow;
        }

        // Interface constraints: same formula as flowgates.
        for (ii, ifd) in self.iface_data.iter().enumerate() {
            let mut flow = 0.0;
            for entry in &ifd.branches {
                let ba = &entry.adm;
                let vi = vm[ba.from];
                let vj = vm[ba.to];
                let theta = va[ba.from] - va[ba.to];
                let (sin_t, cos_t) = theta.sin_cos();
                let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                flow += entry.coeff * pf;
            }
            g[m.iface_con_offset + ii] = flow;
        }

        // Voltage-magnitude slack constraints:
        //   High row i: vm[i] - σ_high[i]  (bounded above by vm_max)
        //   Low  row i: -vm[i] - σ_low[i]  (bounded above by -vm_min)
        if m.has_voltage_slacks() {
            for i in 0..m.n_vm_slack {
                g[m.vm_slack_con_offset + i] = vm[i] - x[m.vm_slack_high_var(i)];
                g[m.vm_slack_con_offset + m.n_vm_slack + i] = -vm[i] - x[m.vm_slack_low_var(i)];
            }
        }

        // Benders cuts: evaluate α^T Pg for each cut.
        let pg = &x[m.pg_offset..m.pg_offset + m.n_gen];
        for (c, cut) in self.cuts.iter().enumerate() {
            let mut val = 0.0_f64;
            for (j, &alpha_j) in cut.alpha.iter().enumerate() {
                val += alpha_j * pg[j];
            }
            g[m.n_con + c] = val;
        }
    }

    fn jacobian_structure(&self) -> (Vec<i32>, Vec<i32>) {
        (self.jac_rows.clone(), self.jac_cols.clone())
    }

    fn eval_jacobian(&self, x: &[f64], values: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, _pg, _qg) = m.extract_voltages_and_dispatch(x);
        values.fill(0.0);

        // Pre-compute per-bus sin/cos (n calls) then use angle-subtraction identities
        // throughout. This replaces a per-edge sin_cos cache (total_nnz allocations)
        // with a compact per-bus array (n_bus elements), eliminating two large heap
        // allocations (sc_sin, sc_cos) on every Ipopt callback.
        let mut sin_va = vec![0.0_f64; m.n_bus];
        let mut cos_va = vec![0.0_f64; m.n_bus];
        for i in 0..m.n_bus {
            (sin_va[i], cos_va[i]) = va[i].sin_cos();
        }
        let mut p_inj = vec![0.0_f64; m.n_bus];
        let mut q_inj = vec![0.0_f64; m.n_bus];
        for i in 0..m.n_bus {
            let row = self.ybus.row(i);
            let vm_i = vm[i];
            let si = sin_va[i];
            let ci = cos_va[i];
            for (k, &j) in row.col_idx.iter().enumerate() {
                let sj = sin_va[j];
                let cj = cos_va[j];
                let s = si * cj - ci * sj;
                let c = ci * cj + si * sj;
                p_inj[i] += vm[j] * (row.g[k] * c + row.b[k] * s);
                q_inj[i] += vm[j] * (row.g[k] * s - row.b[k] * c);
            }
            p_inj[i] *= vm_i;
            q_inj[i] *= vm_i;
        }

        // Precompute per-branch sin/cos (from-side angle = va[f] - va[t]).
        // To-side uses sin(-(θft)) = -sin_ft, cos(-(θft)) = cos_ft — no extra trig.
        let n_br = self.branch_admittances.len();
        let mut br_sin_ft = vec![0.0_f64; n_br];
        let mut br_cos_ft = vec![0.0_f64; n_br];
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let sf = sin_va[ba.from];
            let cf = cos_va[ba.from];
            let st = sin_va[ba.to];
            let ct = cos_va[ba.to];
            br_sin_ft[ci] = sf * ct - cf * st; // sin(va_f - va_t)
            br_cos_ft[ci] = cf * ct + sf * st; // cos(va_f - va_t)
        }

        let mut idx = 0;

        // --- P-balance Jacobian (rows 0..n_bus) ---
        for i in 0..m.n_bus {
            let row_ybus = self.ybus.row(i);
            let vm_i = vm[i];
            let si = sin_va[i];
            let ci = cos_va[i];

            // dP/dVa entries (angle-subtraction identities)
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue; // diagonal handled separately
                }
                // Only if j is a non-slack bus variable
                if m.va_var(j).is_some() {
                    let g_ij = row_ybus.g[k];
                    let b_ij = row_ybus.b[k];
                    let sj = sin_va[j];
                    let cj = cos_va[j];
                    let sin_t = si * cj - ci * sj;
                    let cos_t = ci * cj + si * sj;
                    // dP_i/dVa_j = Vi*Vj*(G_ij*sin(θ_ij) - B_ij*cos(θ_ij))
                    values[idx] = vm_i * vm[j] * (g_ij * sin_t - b_ij * cos_t);
                    idx += 1;
                }
            }
            // dP_i/dVa_i (diagonal) = -Q_i - B_ii * Vi²
            if m.va_var(i).is_some() {
                let b_ii = self.ybus.b(i, i);
                values[idx] = -q_inj[i] - b_ii * vm_i * vm_i;
                idx += 1;
            }

            // dP/dVm entries (angle-subtraction identities)
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];
                let sj = sin_va[j];
                let cj = cos_va[j];
                let sin_t = si * cj - ci * sj;
                let cos_t = ci * cj + si * sj;
                // dP_i/dVm_j = Vi*(G_ij*cos(θ_ij) + B_ij*sin(θ_ij))
                values[idx] = vm_i * (g_ij * cos_t + b_ij * sin_t);
                idx += 1;
            }
            // dP_i/dVm_i (diagonal) = P_i/Vm_i + G_ii*Vm_i
            {
                let g_ii = self.ybus.g(i, i);
                values[idx] = p_inj[i] / vm_i + g_ii * vm_i;
                idx += 1;
            }

            // dP/dPg: -1 for each gen at bus i
            for &_lj in &m.bus_gen_map[i] {
                values[idx] = -1.0;
                idx += 1;
            }
            if m.has_p_bus_balance_slacks() {
                values[idx] = 1.0;
                idx += 1;
                values[idx] = -1.0;
                idx += 1;
            }
            // dP/dQg: 0 (skip — but we still have entries in structure)
            // Actually no, we don't add dP/dQg entries in structure.
        }

        // --- Q-balance Jacobian (rows n_bus..2*n_bus) ---
        for i in 0..m.n_bus {
            let row_ybus = self.ybus.row(i);
            let vm_i = vm[i];
            let si = sin_va[i];
            let ci = cos_va[i];

            // dQ/dVa entries (angle-subtraction identities)
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                if m.va_var(j).is_some() {
                    let g_ij = row_ybus.g[k];
                    let b_ij = row_ybus.b[k];
                    let sj = sin_va[j];
                    let cj = cos_va[j];
                    let sin_t = si * cj - ci * sj;
                    let cos_t = ci * cj + si * sj;
                    // dQ_i/dVa_j = -Vi*Vj*(G_ij*cos(θ_ij) + B_ij*sin(θ_ij))
                    values[idx] = -vm_i * vm[j] * (g_ij * cos_t + b_ij * sin_t);
                    idx += 1;
                }
            }
            // dQ_i/dVa_i (diagonal) = P_i - G_ii * Vi²
            if m.va_var(i).is_some() {
                let g_ii = self.ybus.g(i, i);
                values[idx] = p_inj[i] - g_ii * vm_i * vm_i;
                idx += 1;
            }

            // dQ/dVm entries (angle-subtraction identities)
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let g_ij = row_ybus.g[k];
                let b_ij = row_ybus.b[k];
                let sj = sin_va[j];
                let cj = cos_va[j];
                let sin_t = si * cj - ci * sj;
                let cos_t = ci * cj + si * sj;
                // dQ_i/dVm_j = Vi*(G_ij*sin(θ_ij) - B_ij*cos(θ_ij))
                values[idx] = vm_i * (g_ij * sin_t - b_ij * cos_t);
                idx += 1;
            }
            // dQ_i/dVm_i (diagonal) = Q_i/Vm_i - B_ii*Vm_i
            {
                let b_ii = self.ybus.b(i, i);
                values[idx] = q_inj[i] / vm_i - b_ii * vm_i;
                idx += 1;
            }

            // dQ/dQg: -1 for each gen at bus i
            for &_lj in &m.bus_gen_map[i] {
                values[idx] = -1.0;
                idx += 1;
            }
            if m.has_q_bus_balance_slacks() {
                values[idx] = 1.0;
                idx += 1;
                values[idx] = -1.0;
                idx += 1;
            }
        }

        // --- Branch flow Jacobian (from-side, rows 2*n_bus..) ---
        // g_ci = Pf² + Qf², dg/dx = 2*Pf*dPf/dx + 2*Qf*dQf/dx
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let f = ba.from;
            let t = ba.to;
            let vi = vm[f];
            let vj = vm[t];
            let sin_t = br_sin_ft[ci];
            let cos_t = br_cos_ft[ci];

            let pf = vi * vi * ba.g_ff + vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let qf = -vi * vi * ba.b_ff + vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);

            let dpf_dvaf = vi * vj * (-ba.g_ft * sin_t + ba.b_ft * cos_t);
            let dpf_dvat = vi * vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
            let dpf_dvmf = 2.0 * vi * ba.g_ff + vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dpf_dvmt = vi * (ba.g_ft * cos_t + ba.b_ft * sin_t);

            // dQf/dVaf derivation (S-09): Qf = -Vi²·Bff + Vi·Vj·(Gft·sin θ − Bft·cos θ).
            // With θ = Va_f − Va_t, d(sin θ)/dVa_f = cos θ and d(cos θ)/dVa_f = −sin θ.
            // dQf/dVa_f = Vi·Vj·(Gft·cos θ + Bft·sin θ) > 0 for typical inductive branches.
            // dQf/dVa_t = −Vi·Vj·(Gft·cos θ + Bft·sin θ)  (opposite sign, same magnitude).
            let dqf_dvaf = vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dqf_dvat = -vi * vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let dqf_dvmf = -2.0 * vi * ba.b_ff + vj * (ba.g_ft * sin_t - ba.b_ft * cos_t);
            let dqf_dvmt = vi * (ba.g_ft * sin_t - ba.b_ft * cos_t);

            if m.va_var(f).is_some() {
                values[idx] = 2.0 * (pf * dpf_dvaf + qf * dqf_dvaf);
                idx += 1;
            }
            if m.va_var(t).is_some() {
                values[idx] = 2.0 * (pf * dpf_dvat + qf * dqf_dvat);
                idx += 1;
            }
            values[idx] = 2.0 * (pf * dpf_dvmf + qf * dqf_dvmf);
            idx += 1;
            values[idx] = 2.0 * (pf * dpf_dvmt + qf * dqf_dvmt);
            idx += 1;
            if m.has_thermal_limit_slacks() {
                let sigma = x[m.thermal_slack_from_var(ci)];
                values[idx] = -2.0 * (ba.s_max_pu() + sigma);
                idx += 1;
            }
        }

        // --- Branch flow Jacobian (to-side, rows 2*n_bus + n_br..) ---
        // g_ci = Pt² + Qt², dg/dx = 2*Pt*dPt/dx + 2*Qt*dQt/dx
        // theta_tf = va[t] - va[f] = -(va[f] - va[t]), so sin_tf = -sin_ft, cos_tf = cos_ft.
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let f = ba.from;
            let t = ba.to;
            let vi = vm[f];
            let vj = vm[t];
            let sin_t = -br_sin_ft[ci]; // sin(va_t - va_f) = -sin(va_f - va_t)
            let cos_t = br_cos_ft[ci]; // cos(va_t - va_f) =  cos(va_f - va_t)

            let pt = vj * vj * ba.g_tt + vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let qt = -vj * vj * ba.b_tt + vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);

            // Derivatives of Pt, Qt w.r.t. Va_f, Va_t, Vm_f, Vm_t
            // Note: theta_tf = Va_t - Va_f, so d(theta_tf)/dVa_f = -1, d(theta_tf)/dVa_t = +1
            // dPt/dVa_f = Vj*Vi*(G_tf*sin - B_tf*cos)  (via chain rule, -1 factor)
            let dpt_dvaf = vj * vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);
            // dPt/dVa_t = Vj*Vi*(-G_tf*sin + B_tf*cos)
            let dpt_dvat = vj * vi * (-ba.g_tf * sin_t + ba.b_tf * cos_t);
            // dPt/dVm_f = Vj*(G_tf*cos + B_tf*sin)
            let dpt_dvmf = vj * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            // dPt/dVm_t = 2*Vj*G_tt + Vi*(G_tf*cos + B_tf*sin)
            let dpt_dvmt = 2.0 * vj * ba.g_tt + vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);

            // dQt/dVa_f = -Vj*Vi*(G_tf*cos + B_tf*sin)  (via chain rule)
            let dqt_dvaf = -vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            // dQt/dVa_t = Vj*Vi*(G_tf*cos + B_tf*sin)
            let dqt_dvat = vj * vi * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            // dQt/dVm_f = Vj*(G_tf*sin - B_tf*cos)
            let dqt_dvmf = vj * (ba.g_tf * sin_t - ba.b_tf * cos_t);
            // dQt/dVm_t = -2*Vj*B_tt + Vi*(G_tf*sin - B_tf*cos)
            let dqt_dvmt = -2.0 * vj * ba.b_tt + vi * (ba.g_tf * sin_t - ba.b_tf * cos_t);

            if m.va_var(f).is_some() {
                values[idx] = 2.0 * (pt * dpt_dvaf + qt * dqt_dvaf);
                idx += 1;
            }
            if m.va_var(t).is_some() {
                values[idx] = 2.0 * (pt * dpt_dvat + qt * dqt_dvat);
                idx += 1;
            }
            values[idx] = 2.0 * (pt * dpt_dvmf + qt * dqt_dvmf);
            idx += 1;
            values[idx] = 2.0 * (pt * dpt_dvmt + qt * dqt_dvmt);
            idx += 1;
            if m.has_thermal_limit_slacks() {
                let sigma = x[m.thermal_slack_to_var(ci)];
                values[idx] = -2.0 * (ba.s_max_pu() + sigma);
                idx += 1;
            }
        }

        // --- Angle difference Jacobian (rows 2*n_bus + 2*n_br..) ---
        // g_k = Va_from - Va_to
        // dg_k/dVa_from = +1  (if from is not slack)
        // dg_k/dVa_to   = -1  (if to   is not slack)
        for &(br_idx, _, _) in m.angle_constrained_branches.iter() {
            let br = &self.network.branches[br_idx];
            let f = self.bus_map[&br.from_bus];
            let t = self.bus_map[&br.to_bus];
            if m.va_var(f).is_some() {
                values[idx] = 1.0;
                idx += 1;
            }
            if m.va_var(t).is_some() {
                values[idx] = -1.0;
                idx += 1;
            }
        }

        // --- Tap ratio Jacobian: dP/dτ and dQ/dτ for P/Q balance rows ---
        // For each tap-controllable branch k (variable τ = x[tap_var(k)]):
        //   The contribution to power balance is the delta flow (Pf(τ) - Pf(τ_0)).
        //   The derivative w.r.t. τ is dPf/dτ, which depends on Vf, Vt, θ_ft.
        //
        // Pi-circuit: Pf = Vf²·G_ff(τ) + Vf·Vt·(G_ft(τ)·cos θ + B_ft(τ)·sin θ)
        //   G_ff = gs/τ², dG_ff/dτ = -2·gs/τ³
        //   B_ff = (bs + b/2)/τ², dB_ff/dτ = -2·(bs + b/2)/τ³
        //   G_ft = -(gs·c - bs·s)/τ, dG_ft/dτ = (gs·c - bs·s)/τ²
        //   B_ft = -(gs·s + bs·c)/τ, dB_ft/dτ = (gs·s + bs·c)/τ²
        //   dPf/dτ = Vf²·(-2·gs/τ³) + Vf·Vt·(dG_ft/dτ·cos θ + dB_ft/dτ·sin θ)
        //   dQf/dτ = Vf²·(-(-2·(bs+b/2)/τ³)) + Vf·Vt·(dG_ft/dτ·sin θ - dB_ft/dτ·cos θ)
        //          = Vf²·(2·(bs+b/2)/τ³) + ...
        //
        // To-side:
        //   G_tf = -(gs·c + bs·s)/τ, dG_tf/dτ = (gs·c + bs·s)/τ²
        //   B_tf = (gs·s - bs·c)/τ, dB_tf/dτ = -(gs·s - bs·c)/τ²
        //   G_tt, B_tt don't depend on τ.
        //   dPt/dτ = Vt·Vf·(dG_tf/dτ·cos θ_tf + dB_tf/dτ·sin θ_tf)
        //   dQt/dτ = Vt·Vf·(dG_tf/dτ·sin θ_tf - dB_tf/dτ·cos θ_tf)
        //
        // Jacobian entries: P-balance row fi, Q-balance row n_bus+fi, col=tap_var(k)
        //                   P-balance row ti, Q-balance row n_bus+ti, col=tap_var(k)

        for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];
            let tap_col = m.tap_var(k) as i32;

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };
            let bshunt = bs_ser + br.b / 2.0; // half-line charging

            let shift_rad = br.phase_shift_rad;
            let cos_s = shift_rad.cos();
            let sin_s = shift_rad.sin();

            let tau = x[m.tap_var(k)];
            let tau_sq = tau * tau;
            let tau_cu = tau_sq * tau;

            let vf = vm[fi];
            let vt = vm[ti];
            let theta_ft = va[fi] - va[ti];
            let theta_tf = va[ti] - va[fi];
            let (sin_ft, cos_ft) = theta_ft.sin_cos();
            let (sin_tf, cos_tf) = theta_tf.sin_cos();

            // dG_ff/dτ = -2·gs/τ³,  dB_ff/dτ = -2·(bs+b/2)/τ³
            let dg_ff_dtau = -2.0 * gs / tau_cu;
            let db_ff_dtau = -2.0 * bshunt / tau_cu;

            // dG_ft/dτ = (gs·c - bs·s)/τ²,  dB_ft/dτ = (gs·s + bs·c)/τ²
            let dg_ft_dtau = (gs * cos_s - bs_ser * sin_s) / tau_sq;
            let db_ft_dtau = (gs * sin_s + bs_ser * cos_s) / tau_sq;

            // dG_tf/dτ = (gs·c + bs·s)/τ²,  dB_tf/dτ = -(gs·s - bs·c)/τ²
            let dg_tf_dtau = (gs * cos_s + bs_ser * sin_s) / tau_sq;
            let db_tf_dtau = -(gs * sin_s - bs_ser * cos_s) / tau_sq;

            // dPf/dτ
            let dpf_dtau =
                vf * vf * dg_ff_dtau + vf * vt * (dg_ft_dtau * cos_ft + db_ft_dtau * sin_ft);
            // dQf/dτ = -Vf²·dB_ff/dτ + ...
            let dqf_dtau =
                -vf * vf * db_ff_dtau + vf * vt * (dg_ft_dtau * sin_ft - db_ft_dtau * cos_ft);

            // dPt/dτ
            let dpt_dtau = vt * vf * (dg_tf_dtau * cos_tf + db_tf_dtau * sin_tf);
            // dQt/dτ
            let dqt_dtau = vt * vf * (dg_tf_dtau * sin_tf - db_tf_dtau * cos_tf);

            values[idx] = dpf_dtau; // dP[fi]/dτ_k
            idx += 1;
            values[idx] = dqf_dtau; // dQ[fi]/dτ_k
            idx += 1;
            values[idx] = dpt_dtau; // dP[ti]/dτ_k
            idx += 1;
            values[idx] = dqt_dtau; // dQ[ti]/dτ_k
            idx += 1;
            let _ = tap_col; // used in sparsity structure, not needed here
        }

        // --- Phase shift Jacobian: dP/dθ_s and dQ/dθ_s for P/Q balance rows ---
        //
        // Pi-circuit flows depend on θ_s through G_ft, B_ft, G_tf, B_tf:
        //   G_ft = -(gs·cos θ_s - bs·sin θ_s)/τ
        //   dG_ft/dθ_s = (gs·sin θ_s + bs·cos θ_s)/τ = (gs·sin + bs·cos)/τ
        //   B_ft = -(gs·sin θ_s + bs·cos θ_s)/τ
        //   dB_ft/dθ_s = -(gs·cos θ_s - bs·sin θ_s)/τ = -(gs·cos - bs·sin)/τ
        //   G_tf = -(gs·cos θ_s + bs·sin θ_s)/τ
        //   dG_tf/dθ_s = (gs·sin θ_s - bs·cos θ_s)/τ
        //   B_tf = (gs·sin θ_s - bs·cos θ_s)/τ
        //   dB_tf/dθ_s = (gs·cos θ_s + bs·sin θ_s)/τ
        //
        //   dPf/dθ_s = Vf·Vt·(dG_ft/dθ_s·cos θ + dB_ft/dθ_s·sin θ)
        //   dQf/dθ_s = Vf·Vt·(dG_ft/dθ_s·sin θ - dB_ft/dθ_s·cos θ)
        //   dPt/dθ_s = Vt·Vf·(dG_tf/dθ_s·cos θ_tf + dB_tf/dθ_s·sin θ_tf)
        //   dQt/dθ_s = Vt·Vf·(dG_tf/dθ_s·sin θ_tf - dB_tf/dθ_s·cos θ_tf)
        for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };

            let tau = br.effective_tap();
            let shift_var_rad = x[m.ps_var(k)];
            let cos_s = shift_var_rad.cos();
            let sin_s = shift_var_rad.sin();

            let vf = vm[fi];
            let vt = vm[ti];
            let theta_ft = va[fi] - va[ti];
            let theta_tf = va[ti] - va[fi];
            let (sin_ft, cos_ft) = theta_ft.sin_cos();
            let (sin_tf, cos_tf) = theta_tf.sin_cos();

            // Derivatives of admittance params w.r.t. θ_s
            let dg_ft_dps = (gs * sin_s + bs_ser * cos_s) / tau;
            let db_ft_dps = -(gs * cos_s - bs_ser * sin_s) / tau;
            let dg_tf_dps = (gs * sin_s - bs_ser * cos_s) / tau;
            let db_tf_dps = (gs * cos_s + bs_ser * sin_s) / tau;

            let dpf_dps = vf * vt * (dg_ft_dps * cos_ft + db_ft_dps * sin_ft);
            let dqf_dps = vf * vt * (dg_ft_dps * sin_ft - db_ft_dps * cos_ft);
            let dpt_dps = vt * vf * (dg_tf_dps * cos_tf + db_tf_dps * sin_tf);
            let dqt_dps = vt * vf * (dg_tf_dps * sin_tf - db_tf_dps * cos_tf);

            values[idx] = dpf_dps; // dP[fi]/dθ_s_k
            idx += 1;
            values[idx] = dqf_dps; // dQ[fi]/dθ_s_k
            idx += 1;
            values[idx] = dpt_dps; // dP[ti]/dθ_s_k
            idx += 1;
            values[idx] = dqt_dps; // dQ[ti]/dθ_s_k
            idx += 1;
        }

        // --- Switched shunt Jacobian: dQ[k]/db_sw_i for Q-balance rows ---
        // The switched shunt correction adds -b_sw * Vm[k]² to g[n_bus + k].
        // Derivative: dg[n_bus + k]/db_sw_i = -Vm[k]²
        if self.optimize_switched_shunts {
            for i in 0..m.n_sw {
                let k = m.switched_shunt_bus_idx[i];
                values[idx] = -vm[k] * vm[k]; // dQ[k]/db_sw_i
                idx += 1;
            }
        }

        // --- SVC Jacobian: dQ[k]/db_svc_i = -Vm[k]² ---
        if self.optimize_svc {
            for i in 0..m.n_svc {
                let k = m.svc_devices[i].bus_idx;
                values[idx] = -vm[k] * vm[k];
                idx += 1;
            }
        }

        // --- TCSC Jacobian: dP/dx_comp and dQ/dx_comp at from and to buses ---
        if self.optimize_tcsc {
            for i in 0..m.n_tcsc {
                let tcsc = &m.tcsc_devices[i];
                let x_comp = x[m.tcsc_var(i)];
                let x_eff = tcsc.x_orig - x_comp;
                let r = tcsc.r;
                let z_sq = r * r + x_eff * x_eff;
                let z_sq2 = z_sq * z_sq;

                // Derivatives of series admittance w.r.t. x_comp (dx_eff/dx_comp = -1)
                // g_s = r/z², dg_s/dx_eff = -2r·x_eff/z⁴, dg_s/dx_comp = 2r·x_eff/z⁴
                let dg_s_dx = 2.0 * r * x_eff / z_sq2;
                // b_s = -x_eff/z², db_s/dx_eff = -(z² - 2x_eff²)/z⁴ = (x_eff²-r²)/z⁴
                // db_s/dx_comp = (r²-x_eff²)/z⁴
                let db_s_dx = (r * r - x_eff * x_eff) / z_sq2;

                let tap = tcsc.tap;
                let tap2 = tap * tap;
                let cos_s = tcsc.shift_rad.cos();
                let sin_s = tcsc.shift_rad.sin();

                // Pi-circuit parameter derivatives
                let dg_ff_dx = dg_s_dx / tap2;
                let db_ff_dx = db_s_dx / tap2;
                let dg_ft_dx = -(dg_s_dx * cos_s - db_s_dx * sin_s) / tap;
                let db_ft_dx = -(dg_s_dx * sin_s + db_s_dx * cos_s) / tap;
                let dg_tt_dx = dg_s_dx;
                let db_tt_dx = db_s_dx;
                let dg_tf_dx = -(dg_s_dx * cos_s + db_s_dx * sin_s) / tap;
                let db_tf_dx = (dg_s_dx * sin_s - db_s_dx * cos_s) / tap;

                let fi = tcsc.from_idx;
                let ti = tcsc.to_idx;
                let vf = vm[fi];
                let vt = vm[ti];
                let theta_ft = va[fi] - va[ti];
                let cos_ft = theta_ft.cos();
                let sin_ft = theta_ft.sin();

                // dP_f/dx_comp
                values[idx] =
                    vf * vf * dg_ff_dx + vf * vt * (dg_ft_dx * cos_ft + db_ft_dx * sin_ft);
                idx += 1;
                // dQ_f/dx_comp
                values[idx] =
                    -vf * vf * db_ff_dx + vf * vt * (dg_ft_dx * sin_ft - db_ft_dx * cos_ft);
                idx += 1;
                // dP_t/dx_comp (theta_tf = -theta_ft)
                values[idx] =
                    vt * vt * dg_tt_dx + vt * vf * (dg_tf_dx * cos_ft - db_tf_dx * sin_ft);
                idx += 1;
                // dQ_t/dx_comp
                values[idx] =
                    -vt * vt * db_tt_dx - vt * vf * (dg_tf_dx * sin_ft + db_tf_dx * cos_ft);
                idx += 1;
            }
        }

        // --- HVDC Jacobian ---
        //
        // 1. P-balance rows: dP[ac_bus]/dP_conv_k = +1
        // 2. Q-balance rows: dQ[ac_bus]/dQ_conv_k = +1
        // 3. DC KCL rows:
        //    d(DC_KCL_d)/d(P_conv_k) = 1.0  (for each converter k at DC bus d)
        //    d(DC_KCL_d)/d(V_dc_d) = 2*G_dc(d,d)*V_dc_d + Σ_{j≠d} G_dc(d,j)*V_dc_j
        //    d(DC_KCL_d)/d(V_dc_j) = G_dc(d,j)*V_dc_d  (for j≠d)
        if let Some(ref hvdc) = self.hvdc {
            // P-balance: dP[ac_bus]/dP_conv_k = +1
            for _k in 0..m.n_conv {
                values[idx] = 1.0;
                idx += 1;
            }
            // Q-balance: dQ[ac_bus]/dQ_conv_k = +1
            for _k in 0..m.n_conv {
                values[idx] = 1.0;
                idx += 1;
            }

            // DC KCL Jacobian entries.
            for d in 0..m.n_dc_bus {
                let vd = x[m.vdc_var(d)];

                // d(DC_KCL_d)/d(P_conv_k) = 1.0 for each converter at this DC bus
                for &_k in &hvdc.dc_bus_conv_map[d] {
                    values[idx] = 1.0;
                    idx += 1;
                }

                // d(DC_KCL_d)/d(I_conv_k) = -loss_b - 2·loss_c·I_conv_k
                for &k in &hvdc.dc_bus_conv_map[d] {
                    let c = &hvdc.converters[k];
                    let ic = x[m.iconv_var(k)];
                    values[idx] = -c.loss_linear - 2.0 * c.loss_c * ic;
                    idx += 1;
                }

                // d(DC_KCL_d)/d(V_dc_j) for all DC buses j
                for j in 0..m.n_dc_bus {
                    if j == d {
                        // d(DC_KCL_d)/d(V_dc_d) = 2*G_dc(d,d)*V_dc_d + Σ_{j≠d} G_dc(d,j)*V_dc_j
                        let mut deriv = 2.0 * hvdc.g_dc[d][d] * vd;
                        for jj in 0..m.n_dc_bus {
                            if jj != d {
                                deriv += hvdc.g_dc[d][jj] * x[m.vdc_var(jj)];
                            }
                        }
                        values[idx] = deriv;
                    } else {
                        // d(DC_KCL_d)/d(V_dc_j) = G_dc(d,j)*V_dc_d
                        values[idx] = hvdc.g_dc[d][j] * vd;
                    }
                    idx += 1;
                }
            }

            // Current-definition Jacobian: h_k = P²+Q²-Vm²·I²
            for k in 0..m.n_conv {
                let p = x[m.pconv_var(k)];
                let q = x[m.qconv_var(k)];
                let vm = x[m.vm_offset + m.conv_ac_bus[k]];
                let ic = x[m.iconv_var(k)];
                // dh_k/dP_conv_k = 2·P
                values[idx] = 2.0 * p;
                idx += 1;
                // dh_k/dQ_conv_k = 2·Q
                values[idx] = 2.0 * q;
                idx += 1;
                // dh_k/dVm[ac_bus_k] = -2·Vm·I²
                values[idx] = -2.0 * vm * ic * ic;
                idx += 1;
                // dh_k/dI_conv_k = -2·Vm²·I
                values[idx] = -2.0 * vm * vm * ic;
                idx += 1;
            }

            // DC-control Jacobian entries.
            for k in 0..m.n_conv {
                let c = &hvdc.converters[k];
                match c.dc_control {
                    HvdcDcControlMode::Power => {
                        values[idx] = 1.0;
                        idx += 1;
                        let ic = x[m.iconv_var(k)];
                        values[idx] = -c.loss_linear - 2.0 * c.loss_c * ic;
                        idx += 1;
                    }
                    HvdcDcControlMode::Voltage => {
                        values[idx] = 1.0;
                        idx += 1;
                    }
                }
            }

            // AC-control Jacobian entries.
            for k in 0..m.n_conv {
                let c = &hvdc.converters[k];
                match c.ac_control {
                    HvdcAcControlMode::ReactivePower | HvdcAcControlMode::AcVoltage => {
                        values[idx] = 1.0;
                        idx += 1;
                    }
                }
            }
        }

        // --- HVDC P2P Jacobian values (split-loss) ---
        //
        // Matches `build_jacobian_sparsity` → the P2P block in
        // `src/surge-opf/src/ac/sparsity.rs`. Two entries per link:
        //   dg[from_bus]/dPg_hvdc[k] = +1 + c_pu * Pg
        //   dg[to_bus]/dPg_hvdc[k]   = −1 + c_pu * Pg
        //
        // Differentiating the bus-balance contribution
        //   g[from] += Pg + 0.5*c*Pg²   →   d/dPg = 1 + c*Pg
        //   g[to]   += -Pg + 0.5*c*Pg²  →   d/dPg = -1 + c*Pg
        //
        // For a lossless link (c_pu = 0) the entries collapse to +1
        // and −1, matching the original lossless path byte-for-byte.
        for k in 0..m.n_hvdc_p2p_links {
            let p_hvdc = x[m.hvdc_p2p_var(k)];
            let c_pu = m.hvdc_p2p_loss_c_pu[k];
            values[idx] = 1.0 + c_pu * p_hvdc;
            idx += 1;
            values[idx] = -1.0 + c_pu * p_hvdc;
            idx += 1;
        }

        // --- Storage Jacobian: dP[bus_s]/d_dis[s] = -1, dP[bus_s]/d_ch[s] = +1 ---
        for _s in 0..m.n_sto {
            values[idx] = -1.0;
            idx += 1;
            values[idx] = 1.0;
            idx += 1;
        }
        for k in 0..m.n_dl {
            values[idx] = 1.0;
            idx += 1;
            values[idx] = 1.0;
            idx += 1;
            if m.dispatchable_load_fixed_power_factor[k] {
                values[idx] = -m.dispatchable_load_pf_ratio[k];
                idx += 1;
                values[idx] = 1.0;
                idx += 1;
            }
        }

        // --- PqConstraint Jacobian (D-curve / linear-link / q-headroom) ---
        //   ∂(q_dev − slope·p_dev + sign·q_reserve)/∂p_dev = −slope
        //   ∂(...)/∂q_dev                                   = +1
        //   ∂(...)/∂q_reserve                               = sign (optional)
        //
        // Order MUST match the sparsity declaration in `problem.rs`:
        // two entries per row unconditionally, plus a 3rd entry when
        // the row carries a q-reserve coupling term.
        for c in &self.pq_constraints {
            values[idx] = -c.slope;
            idx += 1;
            values[idx] = 1.0;
            idx += 1;
            if c.q_reserve_var.is_some() {
                values[idx] = c.q_reserve_sign;
                idx += 1;
            }
        }

        // --- Zonal reactive-reserve balance Jacobian ---
        // Row form: Σ q_reserve_participant + q_shortfall ≥ requirement.
        // Every participant contributes a constant `+1.0` entry, and
        // the shortfall slack also contributes `+1.0`. The order MUST
        // match the sparsity declaration in `problem.rs`: all
        // participant entries first, then the shortfall entry, per
        // zone row, in the same order as the `zone_rows` Vec.
        for zone_row in &self.reactive_reserve_plan.zone_rows {
            for _ in &zone_row.participant_cols {
                values[idx] = 1.0;
                idx += 1;
            }
            values[idx] = 1.0;
            idx += 1;
        }

        // --- Flowgate / interface Jacobian ---
        for fgd in self.fg_data.iter().chain(self.iface_data.iter()) {
            let base_idx = idx;
            let n_cols = fgd.jac_cols.len();
            for i in 0..n_cols {
                values[base_idx + i] = 0.0;
            }
            for entry in &fgd.branches {
                let ba = &entry.adm;
                let f = ba.from;
                let t = ba.to;
                let vi = vm[f];
                let vj = vm[t];
                let sf = sin_va[f];
                let cf = cos_va[f];
                let st = sin_va[t];
                let ct = cos_va[t];
                let sin_t = sf * ct - cf * st;
                let cos_t = cf * ct + sf * st;
                let c = entry.coeff;
                let dpf_dvaf = vi * vj * (-ba.g_ft * sin_t + ba.b_ft * cos_t);
                let dpf_dvat = -dpf_dvaf;
                let dpf_dvmf = 2.0 * vi * ba.g_ff + vj * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                let dpf_dvmt = vi * (ba.g_ft * cos_t + ba.b_ft * sin_t);
                if let Some(vaf) = m.va_var(f)
                    && let Some(pos) = fgd.jac_cols.iter().position(|&col| col == vaf)
                {
                    values[base_idx + pos] += c * dpf_dvaf;
                }
                if let Some(vat) = m.va_var(t)
                    && let Some(pos) = fgd.jac_cols.iter().position(|&col| col == vat)
                {
                    values[base_idx + pos] += c * dpf_dvat;
                }
                {
                    let vmf = m.vm_var(f);
                    if let Some(pos) = fgd.jac_cols.iter().position(|&col| col == vmf) {
                        values[base_idx + pos] += c * dpf_dvmf;
                    }
                }
                {
                    let vmt = m.vm_var(t);
                    if let Some(pos) = fgd.jac_cols.iter().position(|&col| col == vmt) {
                        values[base_idx + pos] += c * dpf_dvmt;
                    }
                }
            }
            idx += n_cols;
        }

        // --- Benders cut Jacobian: ∂(α^T Pg)/∂Pg_j = α[j] (constant w.r.t. x) ---
        // One entry per (cut, gen) pair, in the same order as declared in new().
        for cut in &self.cuts {
            for &alpha_j in &cut.alpha {
                values[idx] = alpha_j;
                idx += 1;
            }
        }

        // --- Voltage-magnitude slack constraint Jacobian ---
        // High row i: g = vm[i] - σ_high[i]  →  dg/dVm = +1, dg/dσ_high = -1
        // Low  row i: g = -vm[i] - σ_low[i]  →  dg/dVm = -1, dg/dσ_low = -1
        if m.has_voltage_slacks() {
            for _i in 0..m.n_vm_slack {
                values[idx] = 1.0; // dg_high/dVm
                idx += 1;
                values[idx] = -1.0; // dg_high/dσ_high
                idx += 1;
                values[idx] = -1.0; // dg_low/dVm
                idx += 1;
                values[idx] = -1.0; // dg_low/dσ_low
                idx += 1;
            }
        }

        // --- Angle-difference slack Jacobian (appended at tail) ---
        // The base Va_from/Va_to columns were already written in the
        // angle constraint Jacobian block above; here we only fill the
        // two extra columns for (σ_high, σ_low).
        if m.has_angle_slacks() {
            for _ai in 0..m.n_angle_slack {
                values[idx] = -1.0; // dg/d(sigma_high)
                idx += 1;
                values[idx] = 1.0; // dg/d(sigma_low)
                idx += 1;
            }
        }

        debug_assert_eq!(
            idx,
            values.len(),
            "Jacobian fill mismatch: idx={idx}, expected={}",
            values.len()
        );

        // --- Tap/phase Vm/θ derivative corrections ---
        //
        // The Y-bus block above computed ∂P/∂Vm and ∂P/∂θ for bus-balance
        // rows using BASE branch admittances (the values stored in row_ybus.g
        // / row_ybus.b). For tap-controlled and phase-shifter-controlled
        // branches, the residual adds a "delta correction" `flow(var) -
        // flow(base)` so the bus balance reflects the variable's current
        // value. The Jacobian must include the corresponding ∂/∂Vm and ∂/∂θ
        // derivatives of that delta — otherwise the linearization Ipopt sees
        // is wrong everywhere except at x = x_init (where var = base, so the
        // delta vanishes). Verified via FD check at perturbed points (see
        // `test_acopf_phase_shifter_fd_check_perturbed` and the analogous
        // tap test).
        //
        // For each controlled branch (fi → ti):
        //   Δg_ff = g_ff(var) - g_ff(base)   (tap only; phase doesn't change g_ff)
        //   Δb_ff = b_ff(var) - b_ff(base)
        //   Δg_ft = g_ft(var) - g_ft(base)
        //   Δb_ft = b_ft(var) - b_ft(base)
        //   Δg_tf = g_tf(var) - g_tf(base)
        //   Δb_tf = b_tf(var) - b_tf(base)
        //   (g_tt, b_tt do not depend on tap or shift)
        //
        // ∂dPf/∂Vm[fi] = 2 Vf Δg_ff + Vt (Δg_ft cos θ + Δb_ft sin θ)
        // ∂dPf/∂Vm[ti] = Vf (Δg_ft cos θ + Δb_ft sin θ)
        // ∂dPf/∂θ[fi]  = Vf Vt (-Δg_ft sin θ + Δb_ft cos θ)
        // ∂dPf/∂θ[ti]  = -∂dPf/∂θ[fi]
        // (and analogous expressions for dQf, dPt, dQt; θ_tf = -θ_ft)
        let apply_correction = |row_bal: i32, col: Option<usize>, val: f64, values: &mut [f64]| {
            if let Some(c) = col
                && let Some(&jidx) = self.jac_idx_by_pair.get(&(row_bal, c as i32))
            {
                values[jidx] += val;
            }
        };
        let apply_branch_correction = |br_idx: usize,
                                       dg_ff: f64,
                                       db_ff: f64,
                                       dg_ft: f64,
                                       db_ft: f64,
                                       dg_tf: f64,
                                       db_tf: f64,
                                       values: &mut [f64]| {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];
            let vf = vm[fi];
            let vt = vm[ti];
            let theta_ft = va[fi] - va[ti];
            let (sin_ft, cos_ft) = theta_ft.sin_cos();
            // For to-side: theta_tf = -theta_ft
            let sin_tf = -sin_ft;
            let cos_tf = cos_ft;

            let off_p_ft = dg_ft * cos_ft + db_ft * sin_ft;
            let off_q_ft = dg_ft * sin_ft - db_ft * cos_ft;
            let off_p_tf = dg_tf * cos_tf + db_tf * sin_tf;
            let off_q_tf = dg_tf * sin_tf - db_tf * cos_tf;

            // dPf/dVm[fi] = 2 Vf Δg_ff + Vt * off_p_ft
            // dPf/dVm[ti] = Vf * off_p_ft
            // dPf/dθ[fi]  = Vf Vt * (-Δg_ft sin θ + Δb_ft cos θ) = Vf Vt * dpf_dtheta_ft
            // dPf/dθ[ti]  = -dPf/dθ[fi]
            let dpf_dtheta_ft = -dg_ft * sin_ft + db_ft * cos_ft;
            let dqf_dtheta_ft = dg_ft * cos_ft + db_ft * sin_ft;
            let dpt_dtheta_tf = -dg_tf * sin_tf + db_tf * cos_tf;
            let dqt_dtheta_tf = dg_tf * cos_tf + db_tf * sin_tf;

            let row_pf = fi as i32;
            let row_qf = (m.n_bus + fi) as i32;
            let row_pt = ti as i32;
            let row_qt = (m.n_bus + ti) as i32;
            let vmf_col = Some(m.vm_var(fi));
            let vmt_col = Some(m.vm_var(ti));
            let vaf_col = m.va_var(fi);
            let vat_col = m.va_var(ti);

            // Pf row (fi)
            apply_correction(row_pf, vmf_col, 2.0 * vf * dg_ff + vt * off_p_ft, values);
            apply_correction(row_pf, vmt_col, vf * off_p_ft, values);
            apply_correction(row_pf, vaf_col, vf * vt * dpf_dtheta_ft, values);
            apply_correction(row_pf, vat_col, -vf * vt * dpf_dtheta_ft, values);

            // Qf row (n_bus + fi). Qf = -Vf² b_ff + Vf Vt (g_ft sin θ - b_ft cos θ)
            // dQf/dVm[fi] = -2 Vf Δb_ff + Vt * off_q_ft
            // dQf/dVm[ti] = Vf * off_q_ft
            // dQf/dθ[fi]  = Vf Vt * (g_ft cos θ + b_ft sin θ)·Δ = Vf Vt * dqf_dtheta_ft
            apply_correction(row_qf, vmf_col, -2.0 * vf * db_ff + vt * off_q_ft, values);
            apply_correction(row_qf, vmt_col, vf * off_q_ft, values);
            apply_correction(row_qf, vaf_col, vf * vt * dqf_dtheta_ft, values);
            apply_correction(row_qf, vat_col, -vf * vt * dqf_dtheta_ft, values);

            // Pt row (ti). Pt = Vt² g_tt + Vt Vf (g_tf cos θ_tf + b_tf sin θ_tf)
            // g_tt has no tap/shift dependence → Δg_tt = 0; same for Δb_tt
            // dPt/dVm[ti] = 0 + Vf * off_p_tf
            // dPt/dVm[fi] = Vt * off_p_tf
            // dPt/dθ[ti]  = Vt Vf * dpt_dtheta_tf  (chain: dθ_tf/dθ_ti = +1)
            // dPt/dθ[fi]  = -dPt/dθ[ti]
            apply_correction(row_pt, vmt_col, vf * off_p_tf, values);
            apply_correction(row_pt, vmf_col, vt * off_p_tf, values);
            apply_correction(row_pt, vat_col, vt * vf * dpt_dtheta_tf, values);
            apply_correction(row_pt, vaf_col, -vt * vf * dpt_dtheta_tf, values);

            // Qt row (n_bus + ti)
            apply_correction(row_qt, vmt_col, vf * off_q_tf, values);
            apply_correction(row_qt, vmf_col, vt * off_q_tf, values);
            apply_correction(row_qt, vat_col, vt * vf * dqt_dtheta_tf, values);
            apply_correction(row_qt, vaf_col, -vt * vf * dqt_dtheta_tf, values);
        };

        // Tap-controlled branch corrections
        for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };
            let bshunt = bs_ser + br.b / 2.0;
            let shift_rad = br.phase_shift_rad;
            let cos_s = shift_rad.cos();
            let sin_s = shift_rad.sin();

            let tau_0 = br.effective_tap();
            let tau_var = x[m.tap_var(k)];
            let tau0_sq = tau_0 * tau_0;
            let tau_sq = tau_var * tau_var;

            // Base admittances
            let g_ff0 = gs / tau0_sq;
            let b_ff0 = bshunt / tau0_sq;
            let g_ft0 = -(gs * cos_s - bs_ser * sin_s) / tau_0;
            let b_ft0 = -(gs * sin_s + bs_ser * cos_s) / tau_0;
            let g_tf0 = -(gs * cos_s + bs_ser * sin_s) / tau_0;
            let b_tf0 = (gs * sin_s - bs_ser * cos_s) / tau_0;
            // Variable admittances
            let g_ff_v = gs / tau_sq;
            let b_ff_v = bshunt / tau_sq;
            let g_ft_v = -(gs * cos_s - bs_ser * sin_s) / tau_var;
            let b_ft_v = -(gs * sin_s + bs_ser * cos_s) / tau_var;
            let g_tf_v = -(gs * cos_s + bs_ser * sin_s) / tau_var;
            let b_tf_v = (gs * sin_s - bs_ser * cos_s) / tau_var;

            apply_branch_correction(
                br_idx,
                g_ff_v - g_ff0,
                b_ff_v - b_ff0,
                g_ft_v - g_ft0,
                b_ft_v - b_ft0,
                g_tf_v - g_tf0,
                b_tf_v - b_tf0,
                values,
            );
        }

        // Phase-shifter-controlled branch corrections
        for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };
            let tau = br.effective_tap();
            let shift0_rad = br.phase_shift_rad;
            let shift_var_rad = x[m.ps_var(k)];
            let cos_s0 = shift0_rad.cos();
            let sin_s0 = shift0_rad.sin();
            let cos_sv = shift_var_rad.cos();
            let sin_sv = shift_var_rad.sin();

            // Base admittances (g_ff, b_ff don't depend on shift)
            let g_ft0 = -(gs * cos_s0 - bs_ser * sin_s0) / tau;
            let b_ft0 = -(gs * sin_s0 + bs_ser * cos_s0) / tau;
            let g_tf0 = -(gs * cos_s0 + bs_ser * sin_s0) / tau;
            let b_tf0 = (gs * sin_s0 - bs_ser * cos_s0) / tau;
            // Variable admittances
            let g_ft_v = -(gs * cos_sv - bs_ser * sin_sv) / tau;
            let b_ft_v = -(gs * sin_sv + bs_ser * cos_sv) / tau;
            let g_tf_v = -(gs * cos_sv + bs_ser * sin_sv) / tau;
            let b_tf_v = (gs * sin_sv - bs_ser * cos_sv) / tau;

            apply_branch_correction(
                br_idx,
                0.0,
                0.0, // Δg_ff = 0, Δb_ff = 0 for phase shifter
                g_ft_v - g_ft0,
                b_ft_v - b_ft0,
                g_tf_v - g_tf0,
                b_tf_v - b_tf0,
                values,
            );
        }
    }

    fn has_hessian(&self) -> bool {
        true
    }

    fn hessian_structure(&self) -> (Vec<i32>, Vec<i32>) {
        (self.hess_rows.clone(), self.hess_cols.clone())
    }

    fn eval_hessian(&self, x: &[f64], obj_factor: f64, lambda: &[f64], values: &mut [f64]) {
        let m = &self.mapping;
        let (va, vm, pg, _qg) = m.extract_voltages_and_dispatch(x);
        values.fill(0.0);

        // Pre-compute per-bus sin/cos (n calls) for Y-bus Hessian.
        // Eliminates two large per-callback allocations (sc_sin, sc_cos of size total_nnz).
        let mut sin_va = vec![0.0_f64; m.n_bus];
        let mut cos_va = vec![0.0_f64; m.n_bus];
        for i in 0..m.n_bus {
            (sin_va[i], cos_va[i]) = va[i].sin_cos();
        }

        // Precompute per-branch sin/cos (from-side); to-side uses negated sin.
        let n_br = self.branch_admittances.len();
        let mut br_sin_ft = vec![0.0_f64; n_br];
        let mut br_cos_ft = vec![0.0_f64; n_br];
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let sf = sin_va[ba.from];
            let cf = cos_va[ba.from];
            let st = sin_va[ba.to];
            let ct = cos_va[ba.to];
            br_sin_ft[ci] = sf * ct - cf * st;
            br_cos_ft[ci] = cf * ct + sf * st;
        }

        let hidx = &self.hess_idx;
        let base_sq = self.base_mva * self.base_mva;
        let dt = self.dt_hours;

        // --- Objective Hessian: d²f/dPg_j² (direct-indexed) ---
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &self.network.generators[gi];
            let p_mw = pg[j] * self.base_mva;
            let d2cost = g
                .cost
                .as_ref()
                .expect("generator cost validated in AcOpfProblem::new")
                .second_derivative(p_mw);
            values[hidx.pg_diag[j]] += obj_factor * d2cost * base_sq * dt;
            if let Some(target_p_mw) = self.generator_target_tracking_mw[j] {
                let pair = self.generator_target_tracking_coefficients[j];
                if !pair.is_zero() {
                    // One-sided quadratic Hessian:
                    //   delta > 0 → 2 * α_up
                    //   delta < 0 → 2 * α_down
                    //   delta == 0 → max(α_up, α_down) is used so the
                    //                Hessian is a valid upper bound on
                    //                the subdifferential at the kink.
                    let delta_mw = p_mw - target_p_mw;
                    let penalty = if delta_mw > 0.0 {
                        pair.upward_per_mw2
                    } else if delta_mw < 0.0 {
                        pair.downward_per_mw2
                    } else {
                        pair.upward_per_mw2.max(pair.downward_per_mw2)
                    };
                    if penalty > 0.0 {
                        values[hidx.pg_diag[j]] += obj_factor * 2.0 * penalty * base_sq * dt;
                    }
                }
            }
        }
        for (k, &dl_idx) in self.dispatchable_load_indices.iter().enumerate() {
            let dl = &self.network.market_data.dispatchable_loads[dl_idx];
            let diag = hidx.dl_diag[k];
            if diag != HESS_SKIP {
                values[diag] += obj_factor * dl.cost_model.d2_obj_d_p2(self.base_mva) * dt;
                if let Some(target_p_mw) = self.dispatchable_load_target_tracking_mw[k] {
                    let pair = self.dispatchable_load_target_tracking_coefficients[k];
                    if !pair.is_zero() {
                        let p_served_mw = x[m.dl_var(k)] * self.base_mva;
                        let delta_mw = p_served_mw - target_p_mw;
                        let penalty = if delta_mw > 0.0 {
                            pair.upward_per_mw2
                        } else if delta_mw < 0.0 {
                            pair.downward_per_mw2
                        } else {
                            pair.upward_per_mw2.max(pair.downward_per_mw2)
                        };
                        if penalty > 0.0 {
                            values[diag] += obj_factor * 2.0 * penalty * base_sq * dt;
                        }
                    }
                }
            }
        }

        // --- Power balance Hessian (direct-indexed) ---
        for i in 0..m.n_bus {
            let lp = lambda[i];
            let lq = lambda[m.n_bus + i];

            let row_ybus = self.ybus.row(i);
            let vi = vm[i];
            let g_ii = self.ybus.g(i, i);
            let b_ii = self.ybus.b(i, i);

            // Vm diagonal
            values[hidx.vm_diag[i]] += lp * 2.0 * g_ii + lq * (-2.0 * b_ii);

            // Off-diagonal contributions (direct-indexed, no HashMap)
            let si = sin_va[i];
            let ci = cos_va[i];
            let base_i = self.ybus_row_offsets[i];
            for (k, &j) in row_ybus.col_idx.iter().enumerate() {
                if j == i {
                    continue;
                }
                let gij = row_ybus.g[k];
                let bij = row_ybus.b[k];
                let sj = sin_va[j];
                let cj = cos_va[j];
                let sin_t = si * cj - ci * sj;
                let cos_t = ci * cj + si * sj;

                let aij = gij * cos_t + bij * sin_t;
                let dij = gij * sin_t - bij * cos_t;

                let vj = vm[j];
                let vivj = vi * vj;

                let pos = &hidx.ybus_pb[base_i + k];
                let neg_vivj_a_lp = -lp * vivj * aij;
                let neg_vivj_d_lq = -lq * vivj * dij;
                let combined_neg = neg_vivj_a_lp + neg_vivj_d_lq;

                // [0] (Va_j, Va_j): -Vi*Vj*aij, -Vi*Vj*dij
                if pos[0] != HESS_SKIP {
                    values[pos[0]] += combined_neg;
                }
                // [1] (Va_i, Va_j): +Vi*Vj*aij, +Vi*Vj*dij
                if pos[1] != HESS_SKIP {
                    values[pos[1]] -= combined_neg;
                }
                // [2] (Va_i, Va_i): -Vi*Vj*aij, -Vi*Vj*dij
                if pos[2] != HESS_SKIP {
                    values[pos[2]] += combined_neg;
                }
                // [3] (Vm_i, Va_j): Vj*dij, -Vj*aij
                if pos[3] != HESS_SKIP {
                    values[pos[3]] += lp * (vj * dij) + lq * (-vj * aij);
                }
                // [4] (Vm_j, Va_j): Vi*dij, -Vi*aij
                if pos[4] != HESS_SKIP {
                    values[pos[4]] += lp * (vi * dij) + lq * (-vi * aij);
                }
                // [5] (Vm_j, Va_i): -Vi*dij, Vi*aij
                if pos[5] != HESS_SKIP {
                    values[pos[5]] += lp * (-vi * dij) + lq * (vi * aij);
                }
                // [6] (Vm_i, Va_i): -Vj*dij, Vj*aij
                if pos[6] != HESS_SKIP {
                    values[pos[6]] += lp * (-vj * dij) + lq * (vj * aij);
                }
                // [7] (Vm_i, Vm_j): aij, dij (always valid)
                values[pos[7]] += lp * aij + lq * dij;
            }
        }

        // --- Tap/phase Vm/θ Hessian corrections ---
        //
        // The Y-bus bus-balance Hessian above uses BASE admittances. For
        // tap-controlled and phase-shifter-controlled branches, the
        // residual adds `flow(var) - flow(base)` to bus balance. The
        // Hessian must include the SAME second-derivative correction in the
        // Vm/θ Hessian entries — otherwise Ipopt's Newton step uses a
        // wrong curvature for any iterate where var ≠ base. Mirrors the
        // Jacobian Vm/θ correction added in eval_jacobian and the per-edge
        // Y-bus formulas above.
        //
        // For each controlled branch (fi, ti) the corrections to add are
        // identical in structure to the per-edge formulas above, but with
        // (Δg_ft, Δb_ft) for the (fi, ti) direction and (Δg_tf, Δb_tf) for
        // the (ti, fi) direction. The (Vm_fi, Vm_fi) diagonal also picks
        // up `2 λ_p Δg_ff − 2 λ_q Δb_ff` (tap only — Δg_ff = Δb_ff = 0
        // for phase shifters since g_ff/b_ff don't depend on shift).
        let hm_corr = &self.hess_map;
        let mut add_corr = |r: usize, c: usize, val: f64| {
            let (r, c) = if r >= c { (r, c) } else { (c, r) };
            if let Some(&pos) = hm_corr.get(&(r, c)) {
                values[pos] += val;
            }
        };
        let mut apply_branch_hess_delta = |fi: usize,
                                           ti: usize,
                                           d_g_ff: f64,
                                           d_b_ff: f64,
                                           d_g_ft: f64,
                                           d_b_ft: f64,
                                           d_g_tf: f64,
                                           d_b_tf: f64| {
            let lp_f = lambda[fi];
            let lq_f = lambda[m.n_bus + fi];
            let lp_t = lambda[ti];
            let lq_t = lambda[m.n_bus + ti];

            let _vf = vm[fi];
            let _vt = vm[ti];
            let vmf = m.vm_var(fi);
            let vmt = m.vm_var(ti);
            let vaf_opt = m.va_var(fi);
            let vat_opt = m.va_var(ti);

            // (Vm_fi, Vm_fi) diagonal — only nonzero when Δg_ff/Δb_ff != 0 (tap case).
            if d_g_ff.abs() > 0.0 || d_b_ff.abs() > 0.0 {
                add_corr(vmf, vmf, lp_f * 2.0 * d_g_ff + lq_f * (-2.0 * d_b_ff));
            }
            // (Vm_ti, Vm_ti) diagonal: g_tt, b_tt do not depend on tap or shift,
            // so no correction needed.

            // Helper that mirrors the per-edge Y-bus formulas with (Δg, Δb)
            // playing the role of (g_ij, b_ij). i is the "row" bus, j is
            // its neighbor; the bus-balance row P_i / Q_i picks up the
            // contributions with multipliers λ_p[i], λ_q[i].
            let mut apply_edge = |i: usize, j: usize, dg: f64, db: f64| {
                let lp = lambda[i];
                let lq = lambda[m.n_bus + i];
                if lp.abs() < 1e-30 && lq.abs() < 1e-30 {
                    return;
                }
                let vi = vm[i];
                let vj = vm[j];
                let theta = va[i] - va[j];
                let (sin_t, cos_t) = theta.sin_cos();
                let aij = dg * cos_t + db * sin_t;
                let dij = dg * sin_t - db * cos_t;
                let vivj = vi * vj;

                let vm_i = m.vm_var(i);
                let vm_j = m.vm_var(j);
                let va_i_opt = m.va_var(i);
                let va_j_opt = m.va_var(j);

                let combined_neg = -lp * vivj * aij - lq * vivj * dij;

                // [0] (Va_j, Va_j): += combined_neg
                if let Some(va_j) = va_j_opt {
                    add_corr(va_j, va_j, combined_neg);
                }
                // [1] (Va_i, Va_j): -= combined_neg
                if let (Some(va_i), Some(va_j)) = (va_i_opt, va_j_opt) {
                    add_corr(va_i, va_j, -combined_neg);
                }
                // [2] (Va_i, Va_i): += combined_neg
                if let Some(va_i) = va_i_opt {
                    add_corr(va_i, va_i, combined_neg);
                }
                // [3] (Vm_i, Va_j): lp*vj*dij + lq*(-vj*aij)
                if let Some(va_j) = va_j_opt {
                    add_corr(vm_i, va_j, lp * (vj * dij) + lq * (-vj * aij));
                }
                // [4] (Vm_j, Va_j): lp*vi*dij + lq*(-vi*aij)
                if let Some(va_j) = va_j_opt {
                    add_corr(vm_j, va_j, lp * (vi * dij) + lq * (-vi * aij));
                }
                // [5] (Vm_j, Va_i): lp*(-vi*dij) + lq*(vi*aij)
                if let Some(va_i) = va_i_opt {
                    add_corr(vm_j, va_i, lp * (-vi * dij) + lq * (vi * aij));
                }
                // [6] (Vm_i, Va_i): lp*(-vj*dij) + lq*(vj*aij)
                if let Some(va_i) = va_i_opt {
                    add_corr(vm_i, va_i, lp * (-vj * dij) + lq * (vj * aij));
                }
                // [7] (Vm_i, Vm_j): lp*aij + lq*dij
                add_corr(vm_i, vm_j, lp * aij + lq * dij);
            };

            apply_edge(fi, ti, d_g_ft, d_b_ft);
            apply_edge(ti, fi, d_g_tf, d_b_tf);

            let _ = (vmf, vmt, vaf_opt, vat_opt, lp_f, lq_f, lp_t, lq_t);
        };

        // Tap-controlled branch Hessian corrections
        for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };
            let bshunt = bs_ser + br.b / 2.0;
            let shift_rad = br.phase_shift_rad;
            let cos_s = shift_rad.cos();
            let sin_s = shift_rad.sin();
            let tau_0 = br.effective_tap();
            let tau_var = x[m.tap_var(k)];
            let tau0_sq = tau_0 * tau_0;
            let tau_sq = tau_var * tau_var;
            let g_ff0 = gs / tau0_sq;
            let b_ff0 = bshunt / tau0_sq;
            let g_ft0 = -(gs * cos_s - bs_ser * sin_s) / tau_0;
            let b_ft0 = -(gs * sin_s + bs_ser * cos_s) / tau_0;
            let g_tf0 = -(gs * cos_s + bs_ser * sin_s) / tau_0;
            let b_tf0 = (gs * sin_s - bs_ser * cos_s) / tau_0;
            let g_ff_v = gs / tau_sq;
            let b_ff_v = bshunt / tau_sq;
            let g_ft_v = -(gs * cos_s - bs_ser * sin_s) / tau_var;
            let b_ft_v = -(gs * sin_s + bs_ser * cos_s) / tau_var;
            let g_tf_v = -(gs * cos_s + bs_ser * sin_s) / tau_var;
            let b_tf_v = (gs * sin_s - bs_ser * cos_s) / tau_var;
            apply_branch_hess_delta(
                fi,
                ti,
                g_ff_v - g_ff0,
                b_ff_v - b_ff0,
                g_ft_v - g_ft0,
                b_ft_v - b_ft0,
                g_tf_v - g_tf0,
                b_tf_v - b_tf0,
            );
        }

        // Phase-shifter-controlled branch Hessian corrections
        for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
            let br = &self.network.branches[br_idx];
            let fi = self.bus_map[&br.from_bus];
            let ti = self.bus_map[&br.to_bus];

            let z_sq = br.r * br.r + br.x * br.x;
            let (gs, bs_ser) = if z_sq > 1e-40 {
                (br.r / z_sq, -br.x / z_sq)
            } else {
                (1e6_f64, 0.0_f64)
            };
            let tau = br.effective_tap();
            let shift0_rad = br.phase_shift_rad;
            let shift_var_rad = x[m.ps_var(k)];
            let cos_s0 = shift0_rad.cos();
            let sin_s0 = shift0_rad.sin();
            let cos_sv = shift_var_rad.cos();
            let sin_sv = shift_var_rad.sin();
            let g_ft0 = -(gs * cos_s0 - bs_ser * sin_s0) / tau;
            let b_ft0 = -(gs * sin_s0 + bs_ser * cos_s0) / tau;
            let g_tf0 = -(gs * cos_s0 + bs_ser * sin_s0) / tau;
            let b_tf0 = (gs * sin_s0 - bs_ser * cos_s0) / tau;
            let g_ft_v = -(gs * cos_sv - bs_ser * sin_sv) / tau;
            let b_ft_v = -(gs * sin_sv + bs_ser * cos_sv) / tau;
            let g_tf_v = -(gs * cos_sv + bs_ser * sin_sv) / tau;
            let b_tf_v = (gs * sin_sv - bs_ser * cos_sv) / tau;
            apply_branch_hess_delta(
                fi,
                ti,
                0.0,
                0.0, // Δg_ff = Δb_ff = 0 for phase shifter
                g_ft_v - g_ft0,
                b_ft_v - b_ft0,
                g_tf_v - g_tf0,
                b_tf_v - b_tf0,
            );
        }

        // --- Branch flow Hessian (from-side) ---
        // g_k = Pf² + Qf², ∇²g_k = 2*(∇Pf⊗∇Pf + ∇Qf⊗∇Qf + Pf·∇²Pf + Qf·∇²Qf)
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let mu = lambda[2 * m.n_bus + ci];
            if mu.abs() < 1e-30 {
                continue;
            }

            let vf = vm[ba.from];
            let vt = vm[ba.to];
            let sin_t = br_sin_ft[ci];
            let cos_t = br_cos_ft[ci];

            let a_val = -ba.g_ft * sin_t + ba.b_ft * cos_t;
            let c_val = ba.g_ft * cos_t + ba.b_ft * sin_t;
            let d_val = ba.g_ft * sin_t - ba.b_ft * cos_t;
            let vf_vt = vf * vt;

            let pf = vf * vf * ba.g_ff + vf_vt * (ba.g_ft * cos_t + ba.b_ft * sin_t);
            let qf = -vf * vf * ba.b_ff + vf_vt * (ba.g_ft * sin_t - ba.b_ft * cos_t);

            let dpf = [
                vf_vt * a_val,
                -vf_vt * a_val,
                2.0 * vf * ba.g_ff + vt * c_val,
                vf * c_val,
            ];
            let dqf = [
                vf_vt * c_val,
                -vf_vt * c_val,
                -2.0 * vf * ba.b_ff + vt * d_val,
                vf * d_val,
            ];

            let d2pf = [
                [-vf_vt * c_val, vf_vt * c_val, vt * a_val, vf * a_val],
                [vf_vt * c_val, -vf_vt * c_val, -vt * a_val, -vf * a_val],
                [vt * a_val, -vt * a_val, 2.0 * ba.g_ff, c_val],
                [vf * a_val, -vf * a_val, c_val, 0.0],
            ];
            let d2qf = [
                [vf_vt * a_val, -vf_vt * a_val, vt * c_val, vf * c_val],
                [-vf_vt * a_val, vf_vt * a_val, -vt * c_val, -vf * c_val],
                [vt * c_val, -vt * c_val, -2.0 * ba.b_ff, d_val],
                [vf * c_val, -vf * c_val, d_val, 0.0],
            ];

            // Direct-index: 10 lower-triangle entries per branch
            let pos = &hidx.branch_from[ci];
            let mut flat = 0;
            for a_idx in 0..4 {
                for b_idx in 0..=a_idx {
                    if pos[flat] != HESS_SKIP {
                        let h_val = 2.0
                            * mu
                            * (dpf[a_idx] * dpf[b_idx]
                                + dqf[a_idx] * dqf[b_idx]
                                + pf * d2pf[a_idx][b_idx]
                                + qf * d2qf[a_idx][b_idx]);
                        values[pos[flat]] += h_val;
                    }
                    flat += 1;
                }
            }
            if m.has_thermal_limit_slacks() {
                let sigma_pos = hidx.branch_from_slack_diag[ci];
                if sigma_pos != HESS_SKIP {
                    values[sigma_pos] += -2.0 * mu;
                }
            }
        }

        // --- Branch flow Hessian (to-side) ---
        // g_k = Pt² + Qt², same structure but with to-side admittance
        // theta_tf = va[t] - va[f] = -theta_ft, so sin_tf = -sin_ft, cos_tf = cos_ft.
        for (ci, ba) in self.branch_admittances.iter().enumerate() {
            let mu = lambda[2 * m.n_bus + n_br + ci];
            if mu.abs() < 1e-30 {
                continue;
            }

            let vf = vm[ba.from];
            let vt = vm[ba.to];
            let sin_t = -br_sin_ft[ci]; // sin(va_t - va_f) = -sin(va_f - va_t)
            let cos_t = br_cos_ft[ci]; // cos(va_t - va_f) =  cos(va_f - va_t)

            // To-side shorthands (analogous to from-side but with _tf params)
            let a_val = -ba.g_tf * sin_t + ba.b_tf * cos_t;
            let c_val = ba.g_tf * cos_t + ba.b_tf * sin_t;
            let d_val = ba.g_tf * sin_t - ba.b_tf * cos_t;
            let vf_vt = vf * vt;

            let pt = vt * vt * ba.g_tt + vf_vt * (ba.g_tf * cos_t + ba.b_tf * sin_t);
            let qt = -vt * vt * ba.b_tt + vf_vt * (ba.g_tf * sin_t - ba.b_tf * cos_t);

            // Gradients w.r.t. [θf, θt, Vf, Vt]
            // theta_tf = θt - θf, so d(theta_tf)/dθf = -1, d(theta_tf)/dθt = +1
            // dPt/dθf = Vt*Vf*(G_tf*sin - B_tf*cos) = vf_vt * d_val (via chain rule with -1)
            // dPt/dθt = Vt*Vf*(-G_tf*sin + B_tf*cos) = vf_vt * a_val
            // dPt/dVf = Vt*(G_tf*cos + B_tf*sin) = vt * c_val
            // dPt/dVt = 2*Vt*G_tt + Vf*(G_tf*cos + B_tf*sin) = 2*vt*G_tt + vf*c_val
            let dpt = [
                vf_vt * d_val, // dPt/dθf (note: -(-A) = D where D = gft*sin - bft*cos... let me redo)
                vf_vt * a_val, // dPt/dθt
                vt * c_val,    // dPt/dVf
                2.0 * vt * ba.g_tt + vf * c_val, // dPt/dVt
            ];
            let dqt = [
                -vf_vt * c_val,                   // dQt/dθf
                vf_vt * c_val,                    // dQt/dθt
                vt * d_val,                       // dQt/dVf
                -2.0 * vt * ba.b_tt + vf * d_val, // dQt/dVt
            ];

            // Second derivatives ∇²Pt[a][b] and ∇²Qt[a][b] (4×4 symmetric)
            // Variables: [θf, θt, Vf, Vt], θtf = θt - θf
            // Chain rule: dθtf/dθf = -1, dθtf/dθt = +1
            // Angle-voltage cross terms get sign from chain rule factor:
            //   d²Pt/dθf∂V = (d/dV of dPt/dθtf) × (-1)
            //   d²Pt/dθt∂V = (d/dV of dPt/dθtf) × (+1)
            let d2pt = [
                [-vf_vt * c_val, vf_vt * c_val, -vt * a_val, -vf * a_val],
                [vf_vt * c_val, -vf_vt * c_val, vt * a_val, vf * a_val],
                [-vt * a_val, vt * a_val, 0.0, c_val],
                [-vf * a_val, vf * a_val, c_val, 2.0 * ba.g_tt],
            ];
            let d2qt = [
                [vf_vt * a_val, -vf_vt * a_val, -vt * c_val, -vf * c_val],
                [-vf_vt * a_val, vf_vt * a_val, vt * c_val, vf * c_val],
                [-vt * c_val, vt * c_val, 0.0, d_val],
                [-vf * c_val, vf * c_val, d_val, -2.0 * ba.b_tt],
            ];

            // Direct-index: 10 lower-triangle entries per branch
            let pos = &hidx.branch_to[ci];
            let mut flat = 0;
            for a_idx in 0..4 {
                for b_idx in 0..=a_idx {
                    if pos[flat] != HESS_SKIP {
                        let h_val = 2.0
                            * mu
                            * (dpt[a_idx] * dpt[b_idx]
                                + dqt[a_idx] * dqt[b_idx]
                                + pt * d2pt[a_idx][b_idx]
                                + qt * d2qt[a_idx][b_idx]);
                        values[pos[flat]] += h_val;
                    }
                    flat += 1;
                }
            }
            if m.has_thermal_limit_slacks() {
                let sigma_pos = hidx.branch_to_slack_diag[ci];
                if sigma_pos != HESS_SKIP {
                    values[sigma_pos] += -2.0 * mu;
                }
            }
        }

        // --- HashMap-based add() for remaining Hessian sections (tap/phase/shunt/HVDC) ---
        let hm = &self.hess_map;
        let mut add = |r: usize, c: usize, val: f64| {
            let (r, c) = if r >= c { (r, c) } else { (c, r) };
            if let Some(&pos) = hm.get(&(r, c)) {
                values[pos] += val;
            }
        };

        // --- Tap ratio Hessian ---
        //
        // The Lagrangian contribution from tap variables:
        //   L_tap = Σ_k [ λ_P[fi]·dPf/dτ_k + λ_Q[fi]·dQf/dτ_k
        //                + λ_P[ti]·dPt/dτ_k + λ_Q[ti]·dQt/dτ_k ]
        //
        // Second derivatives (λ_P,λ_Q are the power-balance multipliers):
        //   d²L/(dτ_k²) = Σ λ * d²F/dτ²
        //   d²L/(dτ_k · dVm_f) = Σ λ * d²F/(dτ · dVm_f)
        //   d²L/(dτ_k · dVm_t) = Σ λ * d²F/(dτ · dVm_t)
        //   d²L/(dτ_k · dVa_f) = Σ λ * d²F/(dτ · dVa_f)
        //   d²L/(dτ_k · dVa_t) = Σ λ * d²F/(dτ · dVa_t)
        //
        // Derivation for from-side (all in terms of τ = tau):
        //   dPf/dτ = Vf²·(-2gs/τ³) + Vf·Vt·(dG_ft/dτ·cosθ + dB_ft/dτ·sinθ)
        //
        //   d²Pf/dτ²  = Vf²·(6gs/τ⁴) + Vf·Vt·(d²G_ft/dτ²·cosθ + d²B_ft/dτ²·sinθ)
        //     where d²G_ft/dτ² = -2(gs·c-bs·s)/τ³,  d²B_ft/dτ² = -2(gs·s+bs·c)/τ³
        //
        //   d²Pf/(dτ·dVm_f) = Vf·2·(-2gs/τ³) + Vt·(dG_ft/dτ·cosθ + dB_ft/dτ·sinθ)
        //   d²Pf/(dτ·dVm_t) = Vf·(dG_ft/dτ·cosθ + dB_ft/dτ·sinθ)
        //
        //   d²Pf/(dτ·dVa_f) = Vf·Vt·(dG_ft/dτ·(-sinθ) + dB_ft/dτ·cosθ)  [d/dθ of cosθ=-sinθ]
        //   d²Pf/(dτ·dVa_t) = -d²Pf/(dτ·dVa_f)
        //
        // Similarly for Qf, Pt, Qt (to-side uses τ through G_tf, B_tf only).
        {
            for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
                let br = &self.network.branches[br_idx];
                let fi = self.bus_map[&br.from_bus];
                let ti = self.bus_map[&br.to_bus];

                let lp_f = lambda[fi];
                let lq_f = lambda[m.n_bus + fi];
                let lp_t = lambda[ti];
                let lq_t = lambda[m.n_bus + ti];
                let lsum_f = lp_f + lq_f;
                let lsum_t = lp_t + lq_t;
                if lsum_f.abs() < 1e-30 && lsum_t.abs() < 1e-30 {
                    continue;
                }

                let z_sq = br.r * br.r + br.x * br.x;
                let (gs, bs_ser) = if z_sq > 1e-40 {
                    (br.r / z_sq, -br.x / z_sq)
                } else {
                    (1e6_f64, 0.0_f64)
                };
                let bshunt = bs_ser + br.b / 2.0;

                let shift_rad = br.phase_shift_rad;
                let cos_s = shift_rad.cos();
                let sin_s = shift_rad.sin();

                let tau = x[m.tap_var(k)];
                let tau_sq = tau * tau;
                let tau_cu = tau_sq * tau;
                let tau_4 = tau_cu * tau;

                let vf = vm[fi];
                let vt = vm[ti];
                let theta_ft = va[fi] - va[ti];
                let theta_tf = va[ti] - va[fi];
                let (sin_ft, cos_ft) = theta_ft.sin_cos();
                let (sin_tf, cos_tf) = theta_tf.sin_cos();

                let tau_var = m.tap_var(k);
                let vmf = m.vm_var(fi);
                let vmt = m.vm_var(ti);
                let va_f_opt = m.va_var(fi);
                let va_t_opt = m.va_var(ti);

                // First-order admittance derivatives (reused from Jacobian)
                let dg_ff_dtau = -2.0 * gs / tau_cu;
                let db_ff_dtau = -2.0 * bshunt / tau_cu;
                let dg_ft_dtau = (gs * cos_s - bs_ser * sin_s) / tau_sq;
                let db_ft_dtau = (gs * sin_s + bs_ser * cos_s) / tau_sq;
                let dg_tf_dtau = (gs * cos_s + bs_ser * sin_s) / tau_sq;
                let db_tf_dtau = -(gs * sin_s - bs_ser * cos_s) / tau_sq;

                // Second-order admittance derivatives
                let d2g_ff_dtau2 = 6.0 * gs / tau_4;
                let d2b_ff_dtau2 = 6.0 * bshunt / tau_4;
                let d2g_ft_dtau2 = -2.0 * (gs * cos_s - bs_ser * sin_s) / tau_cu;
                let d2b_ft_dtau2 = -2.0 * (gs * sin_s + bs_ser * cos_s) / tau_cu;
                let d2g_tf_dtau2 = -2.0 * (gs * cos_s + bs_ser * sin_s) / tau_cu;
                let d2b_tf_dtau2 = 2.0 * (gs * sin_s - bs_ser * cos_s) / tau_cu;

                // --- (τ_k, τ_k) diagonal ---
                // d²Pf/dτ² = Vf²·d2G_ff + Vf·Vt·(d2G_ft·cosθ + d2B_ft·sinθ)
                let d2pf_dtau2 = vf * vf * d2g_ff_dtau2
                    + vf * vt * (d2g_ft_dtau2 * cos_ft + d2b_ft_dtau2 * sin_ft);
                let d2qf_dtau2 = -vf * vf * d2b_ff_dtau2
                    + vf * vt * (d2g_ft_dtau2 * sin_ft - d2b_ft_dtau2 * cos_ft);
                let d2pt_dtau2 = vt * vf * (d2g_tf_dtau2 * cos_tf + d2b_tf_dtau2 * sin_tf);
                let d2qt_dtau2 = vt * vf * (d2g_tf_dtau2 * sin_tf - d2b_tf_dtau2 * cos_tf);
                add(
                    tau_var,
                    tau_var,
                    lp_f * d2pf_dtau2 + lq_f * d2qf_dtau2 + lp_t * d2pt_dtau2 + lq_t * d2qt_dtau2,
                );

                // --- (Vm_f, τ_k) cross ---
                // d²Pf/(dτ·dVm_f) = 2·Vf·dG_ff/dτ + Vt·(dG_ft/dτ·cosθ + dB_ft/dτ·sinθ)
                let d2pf_dtau_dvmf =
                    2.0 * vf * dg_ff_dtau + vt * (dg_ft_dtau * cos_ft + db_ft_dtau * sin_ft);
                let d2qf_dtau_dvmf =
                    -2.0 * vf * db_ff_dtau + vt * (dg_ft_dtau * sin_ft - db_ft_dtau * cos_ft);
                let d2pt_dtau_dvmf = vt * (dg_tf_dtau * cos_tf + db_tf_dtau * sin_tf);
                let d2qt_dtau_dvmf = vt * (dg_tf_dtau * sin_tf - db_tf_dtau * cos_tf);
                add(
                    vmf,
                    tau_var,
                    lp_f * d2pf_dtau_dvmf
                        + lq_f * d2qf_dtau_dvmf
                        + lp_t * d2pt_dtau_dvmf
                        + lq_t * d2qt_dtau_dvmf,
                );

                // --- (Vm_t, τ_k) cross ---
                let d2pf_dtau_dvmt = vf * (dg_ft_dtau * cos_ft + db_ft_dtau * sin_ft);
                let d2qf_dtau_dvmt = vf * (dg_ft_dtau * sin_ft - db_ft_dtau * cos_ft);
                // d²Pt/(dτ·dVm_t): Pt = Vt²·G_tt (no τ) + Vt·Vf·(G_tf·cos+B_tf·sin)
                // d²Pt/(dτ·dVm_t) = Vf·(dG_tf/dτ·cosθ_tf + dB_tf/dτ·sinθ_tf)
                let d2pt_dtau_dvmt_correct = vf * (dg_tf_dtau * cos_tf + db_tf_dtau * sin_tf);
                let d2qt_dtau_dvmt = vf * (dg_tf_dtau * sin_tf - db_tf_dtau * cos_tf);
                add(
                    vmt,
                    tau_var,
                    lp_f * d2pf_dtau_dvmt
                        + lq_f * d2qf_dtau_dvmt
                        + lp_t * d2pt_dtau_dvmt_correct
                        + lq_t * d2qt_dtau_dvmt,
                );

                // --- (Va_f, τ_k) cross ---
                // dPf/dτ = ... + Vf·Vt·(dG_ft/dτ·cosθ + dB_ft/dτ·sinθ)
                // d²Pf/(dτ·dVa_f) = Vf·Vt·(-dG_ft/dτ·sinθ + dB_ft/dτ·cosθ)
                let d2pf_dtau_dvaf = vf * vt * (-dg_ft_dtau * sin_ft + db_ft_dtau * cos_ft);
                let d2qf_dtau_dvaf = vf * vt * (dg_ft_dtau * cos_ft + db_ft_dtau * sin_ft);
                // For to-side: d²Pt/(dτ·dVa_f): theta_tf = Va_t - Va_f, dθ_tf/dVa_f = -1
                let d2pt_dtau_dvaf = vt * vf * (dg_tf_dtau * sin_tf - db_tf_dtau * cos_tf); // via -1
                let d2qt_dtau_dvaf = -vt * vf * (dg_tf_dtau * cos_tf + db_tf_dtau * sin_tf);
                if let Some(vaf) = va_f_opt {
                    add(
                        vaf,
                        tau_var,
                        lp_f * d2pf_dtau_dvaf
                            + lq_f * d2qf_dtau_dvaf
                            + lp_t * d2pt_dtau_dvaf
                            + lq_t * d2qt_dtau_dvaf,
                    );
                }

                // --- (Va_t, τ_k) cross --- (opposite sign from Va_f for from-side)
                let d2pf_dtau_dvat = -d2pf_dtau_dvaf;
                let d2qf_dtau_dvat = -d2qf_dtau_dvaf;
                let d2pt_dtau_dvat = -d2pt_dtau_dvaf;
                let d2qt_dtau_dvat = -d2qt_dtau_dvaf;
                if let Some(vat) = va_t_opt {
                    add(
                        vat,
                        tau_var,
                        lp_f * d2pf_dtau_dvat
                            + lq_f * d2qf_dtau_dvat
                            + lp_t * d2pt_dtau_dvat
                            + lq_t * d2qt_dtau_dvat,
                    );
                }
            }

            // --- Phase shift Hessian ---
            //
            // dPf/dθ_s = Vf·Vt·(dG_ft/dθ_s·cosθ_ft + dB_ft/dθ_s·sinθ_ft)
            // Second derivatives w.r.t. θ_s:
            //   d²Pf/dθ_s² = Vf·Vt·(d²G_ft/dθ_s²·cosθ_ft + d²B_ft/dθ_s²·sinθ_ft)
            //   where d²G_ft/dθ_s² = (gs·cos θ_s - bs·sin θ_s)/τ = (original G_ft·(-1)·(-τ/τ))
            //         d²B_ft/dθ_s² = -(gs·sin θ_s + bs·cos θ_s)/τ  [these cycle]
            //
            // Cross with Vm_f: d²Pf/(dθ_s·dVm_f) = Vt·(dG_ft/dθ_s·cosθ_ft + dB_ft/dθ_s·sinθ_ft)
            // Cross with Vm_t: d²Pf/(dθ_s·dVm_t) = Vf·(...)
            // Cross with Va_f: d²Pf/(dθ_s·dVa_f) = Vf·Vt·(dG_ft/dθ_s·(-sinθ_ft) + dB_ft/dθ_s·cosθ_ft)
            // Cross with Va_t: opposite sign
            for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
                let br = &self.network.branches[br_idx];
                let fi = self.bus_map[&br.from_bus];
                let ti = self.bus_map[&br.to_bus];

                let lp_f = lambda[fi];
                let lq_f = lambda[m.n_bus + fi];
                let lp_t = lambda[ti];
                let lq_t = lambda[m.n_bus + ti];
                if (lp_f + lq_f + lp_t + lq_t).abs() < 1e-30 {
                    continue;
                }

                let z_sq = br.r * br.r + br.x * br.x;
                let (gs, bs_ser) = if z_sq > 1e-40 {
                    (br.r / z_sq, -br.x / z_sq)
                } else {
                    (1e6_f64, 0.0_f64)
                };
                let tau = br.effective_tap();

                let shift_var_rad = x[m.ps_var(k)];
                let cos_s = shift_var_rad.cos();
                let sin_s = shift_var_rad.sin();

                let vf = vm[fi];
                let vt = vm[ti];
                let theta_ft = va[fi] - va[ti];
                let theta_tf = va[ti] - va[fi];
                let (sin_ft, cos_ft) = theta_ft.sin_cos();
                let (sin_tf, cos_tf) = theta_tf.sin_cos();

                let ps_var = m.ps_var(k);
                let vmf = m.vm_var(fi);
                let vmt = m.vm_var(ti);
                let va_f_opt = m.va_var(fi);
                let va_t_opt = m.va_var(ti);

                // First-order derivatives of admittance params w.r.t. θ_s
                let dg_ft_dps = (gs * sin_s + bs_ser * cos_s) / tau;
                let db_ft_dps = -(gs * cos_s - bs_ser * sin_s) / tau;
                let dg_tf_dps = (gs * sin_s - bs_ser * cos_s) / tau;
                let db_tf_dps = (gs * cos_s + bs_ser * sin_s) / tau;

                // Second-order derivatives of admittance params w.r.t. θ_s
                // d/dθ_s of (gs·sin+bs·cos)/τ = (gs·cos-bs·sin)/τ
                let d2g_ft_dps2 = (gs * cos_s - bs_ser * sin_s) / tau;
                // d/dθ_s of -(gs·cos-bs·sin)/τ = (gs·sin+bs·cos)/τ
                let d2b_ft_dps2 = (gs * sin_s + bs_ser * cos_s) / tau;
                let d2g_tf_dps2 = (gs * cos_s + bs_ser * sin_s) / tau;
                let d2b_tf_dps2 = -(gs * sin_s - bs_ser * cos_s) / tau;

                // (θ_s, θ_s) diagonal
                let d2pf_dps2 = vf * vt * (d2g_ft_dps2 * cos_ft + d2b_ft_dps2 * sin_ft);
                let d2qf_dps2 = vf * vt * (d2g_ft_dps2 * sin_ft - d2b_ft_dps2 * cos_ft);
                let d2pt_dps2 = vt * vf * (d2g_tf_dps2 * cos_tf + d2b_tf_dps2 * sin_tf);
                let d2qt_dps2 = vt * vf * (d2g_tf_dps2 * sin_tf - d2b_tf_dps2 * cos_tf);
                add(
                    ps_var,
                    ps_var,
                    lp_f * d2pf_dps2 + lq_f * d2qf_dps2 + lp_t * d2pt_dps2 + lq_t * d2qt_dps2,
                );

                // (Vm_f, θ_s) cross
                let d2pf_dps_dvmf = vt * (dg_ft_dps * cos_ft + db_ft_dps * sin_ft);
                let d2qf_dps_dvmf = vt * (dg_ft_dps * sin_ft - db_ft_dps * cos_ft);
                let d2pt_dps_dvmf = vt * (dg_tf_dps * cos_tf + db_tf_dps * sin_tf);
                let d2qt_dps_dvmf = vt * (dg_tf_dps * sin_tf - db_tf_dps * cos_tf);
                add(
                    vmf,
                    ps_var,
                    lp_f * d2pf_dps_dvmf
                        + lq_f * d2qf_dps_dvmf
                        + lp_t * d2pt_dps_dvmf
                        + lq_t * d2qt_dps_dvmf,
                );

                // (Vm_t, θ_s) cross
                let d2pf_dps_dvmt = vf * (dg_ft_dps * cos_ft + db_ft_dps * sin_ft);
                let d2qf_dps_dvmt = vf * (dg_ft_dps * sin_ft - db_ft_dps * cos_ft);
                let d2pt_dps_dvmt = vf * (dg_tf_dps * cos_tf + db_tf_dps * sin_tf);
                let d2qt_dps_dvmt = vf * (dg_tf_dps * sin_tf - db_tf_dps * cos_tf);
                add(
                    vmt,
                    ps_var,
                    lp_f * d2pf_dps_dvmt
                        + lq_f * d2qf_dps_dvmt
                        + lp_t * d2pt_dps_dvmt
                        + lq_t * d2qt_dps_dvmt,
                );

                // (Va_f, θ_s) cross
                let d2pf_dps_dvaf = vf * vt * (-dg_ft_dps * sin_ft + db_ft_dps * cos_ft);
                let d2qf_dps_dvaf = vf * vt * (dg_ft_dps * cos_ft + db_ft_dps * sin_ft);
                let d2pt_dps_dvaf = vt * vf * (dg_tf_dps * sin_tf - db_tf_dps * cos_tf);
                let d2qt_dps_dvaf = -vt * vf * (dg_tf_dps * cos_tf + db_tf_dps * sin_tf);
                if let Some(vaf) = va_f_opt {
                    add(
                        vaf,
                        ps_var,
                        lp_f * d2pf_dps_dvaf
                            + lq_f * d2qf_dps_dvaf
                            + lp_t * d2pt_dps_dvaf
                            + lq_t * d2qt_dps_dvaf,
                    );
                }

                // (Va_t, θ_s) cross: opposite sign from (Va_f, θ_s) for from-side
                if let Some(vat) = va_t_opt {
                    add(
                        vat,
                        ps_var,
                        lp_f * (-d2pf_dps_dvaf)
                            + lq_f * (-d2qf_dps_dvaf)
                            + lp_t * (-d2pt_dps_dvaf)
                            + lq_t * (-d2qt_dps_dvaf),
                    );
                }
            }

            // --- Switched shunt Hessian ---
            // g[n_bus+k] += -b_sw_i * Vm[k]²
            // d²L/(db_sw_i · dVm[k]) = λ_Q[k] * (-2·Vm[k])
            if self.optimize_switched_shunts {
                for i in 0..m.n_sw {
                    let k = m.switched_shunt_bus_idx[i];
                    let lq_k = lambda[m.n_bus + k];
                    if lq_k.abs() > 1e-30 {
                        let sw_var = m.sw_var(i);
                        let vmk = m.vm_var(k);
                        // d²L/(dVm[k] · db_sw_i) = λ_Q[k] * (-2·Vm[k])
                        add(vmk, sw_var, lq_k * (-2.0 * vm[k]));
                        // (b_sw_i, b_sw_i) diagonal: value is 0 (constraint linear in b_sw)
                        // but entry is in hess_map; just add 0 — no-op, already zero-init
                        let _ = sw_var; // suppress warning
                    }
                }
            }

            // --- SVC Hessian ---
            // Same structure as switched shunts: d²L/(db_svc·dVm[k]) = λ_Q[k] * (-2·Vm[k])
            if self.optimize_svc {
                for i in 0..m.n_svc {
                    let k = m.svc_devices[i].bus_idx;
                    let lq_k = lambda[m.n_bus + k];
                    if lq_k.abs() > 1e-30 {
                        let svc_v = m.svc_var(i);
                        let vmk = m.vm_var(k);
                        add(vmk, svc_v, lq_k * (-2.0 * vm[k]));
                    }
                }
            }

            // --- TCSC Hessian ---
            // Second derivatives of power balance w.r.t. x_comp and cross-terms
            // with Vm and Va at from/to buses.
            if self.optimize_tcsc {
                for i in 0..m.n_tcsc {
                    let tcsc = &m.tcsc_devices[i];
                    let x_comp = x[m.tcsc_var(i)];
                    let x_eff = tcsc.x_orig - x_comp;
                    let r = tcsc.r;
                    let z_sq = r * r + x_eff * x_eff;
                    let z_sq2 = z_sq * z_sq;
                    let z_sq3 = z_sq2 * z_sq;

                    let fi = tcsc.from_idx;
                    let ti = tcsc.to_idx;
                    let vf = vm[fi];
                    let vt = vm[ti];
                    let theta_ft = va[fi] - va[ti];
                    let cos_ft = theta_ft.cos();
                    let sin_ft = theta_ft.sin();
                    let tap = tcsc.tap;
                    let tap2 = tap * tap;
                    let cos_s = tcsc.shift_rad.cos();
                    let sin_s = tcsc.shift_rad.sin();

                    let lp_f = lambda[fi];
                    let lq_f = lambda[m.n_bus + fi];
                    let lp_t = lambda[ti];
                    let lq_t = lambda[m.n_bus + ti];

                    // Second derivatives of series admittance w.r.t. x_comp
                    let d2g_s = 2.0 * r * (3.0 * x_eff * x_eff - r * r) / z_sq3;
                    let d2b_s = 2.0 * x_eff * (3.0 * r * r - x_eff * x_eff) / z_sq3;

                    // First derivatives (needed for cross-terms)
                    let dg_s_dx = 2.0 * r * x_eff / z_sq2;
                    let db_s_dx = (r * r - x_eff * x_eff) / z_sq2;

                    // Pi-circuit second derivatives
                    let d2g_ff = d2g_s / tap2;
                    let d2b_ff = d2b_s / tap2;
                    let d2g_ft = -(d2g_s * cos_s - d2b_s * sin_s) / tap;
                    let d2b_ft = -(d2g_s * sin_s + d2b_s * cos_s) / tap;
                    let d2g_tt = d2g_s;
                    let d2b_tt = d2b_s;
                    let d2g_tf = -(d2g_s * cos_s + d2b_s * sin_s) / tap;
                    let d2b_tf = (d2g_s * sin_s - d2b_s * cos_s) / tap;

                    // Pi-circuit first derivatives for cross-terms
                    let dg_ff_dx = dg_s_dx / tap2;
                    let db_ff_dx = db_s_dx / tap2;
                    let dg_ft_dx = -(dg_s_dx * cos_s - db_s_dx * sin_s) / tap;
                    let db_ft_dx = -(dg_s_dx * sin_s + db_s_dx * cos_s) / tap;
                    let dg_tt_dx = dg_s_dx;
                    let db_tt_dx = db_s_dx;
                    let dg_tf_dx = -(dg_s_dx * cos_s + db_s_dx * sin_s) / tap;
                    let db_tf_dx = (dg_s_dx * sin_s - db_s_dx * cos_s) / tap;

                    // (x_comp, x_comp) diagonal: d²L/dx² from power balance
                    let d2p_f = vf * vf * d2g_ff + vf * vt * (d2g_ft * cos_ft + d2b_ft * sin_ft);
                    let d2q_f = -vf * vf * d2b_ff + vf * vt * (d2g_ft * sin_ft - d2b_ft * cos_ft);
                    let d2p_t = vt * vt * d2g_tt + vt * vf * (d2g_tf * cos_ft - d2b_tf * sin_ft);
                    let d2q_t = -vt * vt * d2b_tt - vt * vf * (d2g_tf * sin_ft + d2b_tf * cos_ft);

                    let xc_v = m.tcsc_var(i);
                    add(
                        xc_v,
                        xc_v,
                        lp_f * d2p_f + lq_f * d2q_f + lp_t * d2p_t + lq_t * d2q_t,
                    );

                    // (Vm_f, x_comp): d²L/(dVm_f · dx_comp)
                    let d2_vmf_xc_pf =
                        2.0 * vf * dg_ff_dx + vt * (dg_ft_dx * cos_ft + db_ft_dx * sin_ft);
                    let d2_vmf_xc_qf =
                        -2.0 * vf * db_ff_dx + vt * (dg_ft_dx * sin_ft - db_ft_dx * cos_ft);
                    let d2_vmf_xc_pt = vt * (dg_tf_dx * cos_ft - db_tf_dx * sin_ft);
                    let d2_vmf_xc_qt = -vt * (dg_tf_dx * sin_ft + db_tf_dx * cos_ft);
                    add(
                        m.vm_var(fi),
                        xc_v,
                        lp_f * d2_vmf_xc_pf
                            + lq_f * d2_vmf_xc_qf
                            + lp_t * d2_vmf_xc_pt
                            + lq_t * d2_vmf_xc_qt,
                    );

                    // (Vm_t, x_comp)
                    let d2_vmt_xc_pf = vf * (dg_ft_dx * cos_ft + db_ft_dx * sin_ft);
                    let d2_vmt_xc_qf = vf * (dg_ft_dx * sin_ft - db_ft_dx * cos_ft);
                    let d2_vmt_xc_pt =
                        2.0 * vt * dg_tt_dx + vf * (dg_tf_dx * cos_ft - db_tf_dx * sin_ft);
                    let d2_vmt_xc_qt =
                        -2.0 * vt * db_tt_dx - vf * (dg_tf_dx * sin_ft + db_tf_dx * cos_ft);
                    add(
                        m.vm_var(ti),
                        xc_v,
                        lp_f * d2_vmt_xc_pf
                            + lq_f * d2_vmt_xc_qf
                            + lp_t * d2_vmt_xc_pt
                            + lq_t * d2_vmt_xc_qt,
                    );

                    // (Va_f, x_comp): d/dVa_f of dP_f/dx = Vf*Vt*(-dg_ft·sin+db_ft·cos)
                    let d2_vaf_pf = vf * vt * (-dg_ft_dx * sin_ft + db_ft_dx * cos_ft);
                    let d2_vaf_qf = vf * vt * (dg_ft_dx * cos_ft + db_ft_dx * sin_ft);
                    let d2_vaf_pt = vt * vf * (-dg_tf_dx * sin_ft - db_tf_dx * cos_ft);
                    let d2_vaf_qt = -vt * vf * (dg_tf_dx * cos_ft - db_tf_dx * sin_ft);
                    if let Some(va_f_var) = m.va_var(fi) {
                        add(
                            va_f_var,
                            xc_v,
                            lp_f * d2_vaf_pf
                                + lq_f * d2_vaf_qf
                                + lp_t * d2_vaf_pt
                                + lq_t * d2_vaf_qt,
                        );
                    }

                    // (Va_t, x_comp): d/dVa_t = -(d/dVa_f) for all terms
                    // because theta_ft = va_f - va_t and the only dependency is via cos/sin(theta_ft)
                    if let Some(va_t_var) = m.va_var(ti) {
                        add(
                            va_t_var,
                            xc_v,
                            -(lp_f * d2_vaf_pf
                                + lq_f * d2_vaf_qf
                                + lp_t * d2_vaf_pt
                                + lq_t * d2_vaf_qt),
                        );
                    }
                }
            }

            // --- HVDC DC KCL Hessian ---
            //
            // DC KCL constraint at bus d:
            //   g_dc[d] = Σ_{k∈d} (P_conv_k - loss_a - loss_b·I_k - loss_c·I_k²)
            //           + Σ_j G_dc(d,j)*V_dc_d*V_dc_j = 0
            //
            // P_conv terms are linear → zero second derivatives.
            // loss_b·I term is linear → zero second derivative.
            // loss_c·I² term: d²/d(I_k)² = -2·loss_c
            // DC network term: d²/d(V_d)² = 2·G(d,d), d²/d(V_d)d(V_j) = G(d,j)
            if let Some(ref hvdc) = self.hvdc {
                for d in 0..m.n_dc_bus {
                    let lam_d = lambda[m.dc_kcl_row_offset + d];
                    if lam_d.abs() < 1e-30 {
                        continue;
                    }

                    let vdc_d = m.vdc_var(d);
                    // Diagonal: d²g/d(V_dc_d)² = 2*G_dc(d,d)
                    add(vdc_d, vdc_d, lam_d * 2.0 * hvdc.g_dc[d][d]);

                    // Off-diagonal: d²g/d(V_dc_d)d(V_dc_j) = G_dc(d,j)
                    for j in 0..m.n_dc_bus {
                        if j != d && hvdc.g_dc[d][j].abs() > 1e-30 {
                            add(m.vdc_var(d), m.vdc_var(j), lam_d * hvdc.g_dc[d][j]);
                        }
                    }

                    // I_conv diagonal from loss_c: d²(-loss_c·I²)/dI² = -2·loss_c
                    for &k in &hvdc.dc_bus_conv_map[d] {
                        let c = &hvdc.converters[k];
                        if c.loss_c.abs() > 1e-30 {
                            add(m.iconv_var(k), m.iconv_var(k), lam_d * (-2.0 * c.loss_c));
                        }
                    }
                }

                // --- Current-definition Hessian ---
                //
                // h_k = P_k² + Q_k² - Vm_k² · I_k² = 0
                //
                // Second derivatives (with multiplier μ_k):
                //   d²h/dP² = 2         → μ_k * 2
                //   d²h/dQ² = 2         → μ_k * 2
                //   d²h/dVm² = -2·I²    → μ_k * (-2·I²)
                //   d²h/dI² = -2·Vm²    → μ_k * (-2·Vm²)
                //   d²h/(dVm·dI) = -4·Vm·I → μ_k * (-4·Vm·I)
                for k in 0..m.n_conv {
                    let mu_k = lambda[m.iconv_eq_row_offset + k];
                    if mu_k.abs() < 1e-30 {
                        continue;
                    }
                    let vm = x[m.vm_offset + m.conv_ac_bus[k]];
                    let ic = x[m.iconv_var(k)];

                    add(m.pconv_var(k), m.pconv_var(k), mu_k * 2.0);
                    add(m.qconv_var(k), m.qconv_var(k), mu_k * 2.0);
                    add(
                        m.vm_var(m.conv_ac_bus[k]),
                        m.vm_var(m.conv_ac_bus[k]),
                        mu_k * (-2.0 * ic * ic),
                    );
                    add(m.iconv_var(k), m.iconv_var(k), mu_k * (-2.0 * vm * vm));
                    add(
                        m.vm_var(m.conv_ac_bus[k]),
                        m.iconv_var(k),
                        mu_k * (-4.0 * vm * ic),
                    );
                }

                // DC power-control Hessian:
                //   p_conv - loss_a - loss_b·I - loss_c·I² - p_set = 0
                // contributes only d²/dI² = -2·loss_c.
                for k in 0..m.n_conv {
                    let c = &hvdc.converters[k];
                    if c.dc_control != HvdcDcControlMode::Power {
                        continue;
                    }
                    let lam_k = lambda[m.dc_control_row(k)];
                    if lam_k.abs() < 1e-30 || c.loss_c.abs() < 1e-30 {
                        continue;
                    }
                    add(m.iconv_var(k), m.iconv_var(k), lam_k * (-2.0 * c.loss_c));
                }
            }
        }

        // --- Flowgate / interface Hessian ---
        // g = Σ_k coeff_k * Pf_k: ∂²L/∂x∂y += λ * Σ_k coeff_k * ∂²Pf_k/∂x∂y
        for (di, fgd) in self
            .fg_data
            .iter()
            .chain(self.iface_data.iter())
            .enumerate()
        {
            let n_fg = self.fg_data.len();
            let lam_row = if di < n_fg {
                m.fg_con_offset + di
            } else {
                m.iface_con_offset + (di - n_fg)
            };
            let mu = lambda[lam_row];
            if mu.abs() < 1e-30 {
                continue;
            }
            for entry in &fgd.branches {
                let ba = &entry.adm;
                let f = ba.from;
                let t = ba.to;
                let vi = vm[f];
                let vj = vm[t];
                let sf = sin_va[f];
                let cf = cos_va[f];
                let st = sin_va[t];
                let ct = cos_va[t];
                let sin_t = sf * ct - cf * st;
                let cos_t = cf * ct + sf * st;
                let c_val = ba.g_ft * cos_t + ba.b_ft * sin_t;
                let a_val = -ba.g_ft * sin_t + ba.b_ft * cos_t;
                let scale = mu * entry.coeff;
                // VaVa
                if let Some(vaf) = m.va_var(f) {
                    add(vaf, vaf, scale * (-vi * vj * c_val));
                    if let Some(vat) = m.va_var(t) {
                        add(vaf, vat, scale * (vi * vj * c_val));
                    }
                }
                if let Some(vat) = m.va_var(t) {
                    add(vat, vat, scale * (-vi * vj * c_val));
                }
                // VmVa cross
                let vmf = m.vm_var(f);
                let vmt = m.vm_var(t);
                if let Some(vaf) = m.va_var(f) {
                    add(vmf, vaf, scale * (vj * a_val));
                    add(vmt, vaf, scale * (vi * a_val));
                }
                if let Some(vat) = m.va_var(t) {
                    add(vmf, vat, scale * (-vj * a_val));
                    add(vmt, vat, scale * (-vi * a_val));
                }
                // VmVm
                add(vmf, vmf, scale * 2.0 * ba.g_ff);
                add(vmf, vmt, scale * c_val);
            }
        }

        // --- HVDC P2P Hessian (split-loss quadratic) ---
        //
        // The bus-balance contribution at each terminal is
        //   g[from] += Pg + 0.5*c*Pg²   (row = from-bus P balance)
        //   g[to]   += -Pg + 0.5*c*Pg²  (row = to-bus   P balance)
        // so the second derivative with respect to the HVDC P variable
        // on each row is `d²g/dPg² = c_pu`. The Lagrangian Hessian
        // contribution at the `(hvdc_var, hvdc_var)` diagonal slot is
        // the sum of both rows' multipliers times `c_pu`:
        //
        //   ∂²L/∂Pg² = c_pu * (λ[from_row] + λ[to_row])
        //
        // Lossless links have `c_pu = 0` and contribute nothing —
        // skip them so the `hess_map` lookup doesn't need a diagonal
        // entry that was never created during sparsity build.
        for k in 0..m.n_hvdc_p2p_links {
            let c_pu = m.hvdc_p2p_loss_c_pu[k];
            if c_pu.abs() < 1e-20 {
                continue;
            }
            let from_row = m.hvdc_p2p_from_bus_idx[k];
            let to_row = m.hvdc_p2p_to_bus_idx[k];
            let lam = lambda[from_row] + lambda[to_row];
            let v = m.hvdc_p2p_var(k);
            add(v, v, c_pu * lam);
        }
    }
}
