// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Jacobian and Hessian sparsity structure builders for AC-OPF.
//!
//! These functions pre-compute the sparsity pattern (row/col index arrays)
//! that Ipopt uses for sparse Jacobian and Hessian evaluation, plus the
//! initial point construction for the NLP solver.

use std::collections::HashMap;

use surge_ac::matrix::ybus::YBus;
use surge_network::Network;

use super::mapping::AcOpfMapping;
use super::types::{BranchAdmittance, WarmStart, branch_flow_from, branch_flow_to};

fn re_reference_warm_start_angles(
    network: &Network,
    mapping: &AcOpfMapping,
    angles_rad: &[f64],
) -> Vec<f64> {
    let mut referenced_angles: Vec<f64> = network
        .buses
        .iter()
        .enumerate()
        .map(|(bus_idx, bus)| {
            angles_rad
                .get(bus_idx)
                .copied()
                .unwrap_or(bus.voltage_angle_rad)
        })
        .collect();

    let reference_bus_idx = (0..mapping.n_bus)
        .find(|&bus_idx| {
            mapping.va_var(bus_idx).is_none()
                && network.buses[bus_idx].bus_type == surge_network::network::BusType::Slack
        })
        .or_else(|| (0..mapping.n_bus).find(|&bus_idx| mapping.va_var(bus_idx).is_none()));
    let Some(reference_bus_idx) = reference_bus_idx else {
        return referenced_angles;
    };

    let reference_angle_rad = referenced_angles[reference_bus_idx];
    for angle_rad in &mut referenced_angles {
        *angle_rad -= reference_angle_rad;
    }
    referenced_angles
}

// ---------------------------------------------------------------------------
// Jacobian sparsity structure
// ---------------------------------------------------------------------------

pub(super) fn build_jacobian_sparsity(
    mapping: &AcOpfMapping,
    network: &Network,
    ybus: &YBus,
    bus_map: &HashMap<u32, usize>,
    branch_admittances: &[BranchAdmittance],
    hvdc: Option<&super::hvdc::HvdcNlpData>,
    hvdc_p2p: Option<&super::hvdc::HvdcP2PNlpData>,
) -> (Vec<i32>, Vec<i32>) {
    let m = mapping;
    let mut rows = Vec::new();
    let mut cols = Vec::new();

    // --- P-balance rows (0..n_bus) ---
    for i in 0..m.n_bus {
        let row_ybus = ybus.row(i);
        let row = i as i32;

        // dP_i/dVa_j for off-diagonal j (non-slack)
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            if let Some(va_col) = m.va_var(j) {
                rows.push(row);
                cols.push(va_col as i32);
            }
        }
        // dP_i/dVa_i (diagonal, if non-slack)
        if let Some(va_col) = m.va_var(i) {
            rows.push(row);
            cols.push(va_col as i32);
        }

        // dP_i/dVm_j for off-diagonal j
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            rows.push(row);
            cols.push(m.vm_var(j) as i32);
        }
        // dP_i/dVm_i (diagonal)
        rows.push(row);
        cols.push(m.vm_var(i) as i32);

        // dP_i/dPg_j for each gen at bus i
        for &lj in &m.bus_gen_map[i] {
            rows.push(row);
            cols.push(m.pg_var(lj) as i32);
        }
        if m.has_p_bus_balance_slacks() {
            rows.push(row);
            cols.push(m.p_balance_slack_pos_var(i) as i32);
            rows.push(row);
            cols.push(m.p_balance_slack_neg_var(i) as i32);
        }
    }

    // --- Q-balance rows (n_bus..2*n_bus) ---
    for i in 0..m.n_bus {
        let row_ybus = ybus.row(i);
        let row = (m.n_bus + i) as i32;

        // dQ_i/dVa_j off-diagonal (non-slack)
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            if let Some(va_col) = m.va_var(j) {
                rows.push(row);
                cols.push(va_col as i32);
            }
        }
        // dQ_i/dVa_i diagonal
        if let Some(va_col) = m.va_var(i) {
            rows.push(row);
            cols.push(va_col as i32);
        }

        // dQ_i/dVm_j off-diagonal
        for &j in row_ybus.col_idx {
            if j == i {
                continue;
            }
            rows.push(row);
            cols.push(m.vm_var(j) as i32);
        }
        // dQ_i/dVm_i diagonal
        rows.push(row);
        cols.push(m.vm_var(i) as i32);

        // dQ_i/dQg_j for each gen at bus i
        for &lj in &m.bus_gen_map[i] {
            rows.push(row);
            cols.push(m.qg_var(lj) as i32);
        }
        if m.has_q_bus_balance_slacks() {
            rows.push(row);
            cols.push(m.q_balance_slack_pos_var(i) as i32);
            rows.push(row);
            cols.push(m.q_balance_slack_neg_var(i) as i32);
        }
    }

    // --- Branch flow rows (from-side): rows 2*n_bus.. ---
    let n_br = branch_admittances.len();
    for (ci, ba) in branch_admittances.iter().enumerate() {
        let row = (2 * m.n_bus + ci) as i32;
        if let Some(va_from) = m.va_var(ba.from) {
            rows.push(row);
            cols.push(va_from as i32);
        }
        if let Some(va_to) = m.va_var(ba.to) {
            rows.push(row);
            cols.push(va_to as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(ba.from) as i32);
        rows.push(row);
        cols.push(m.vm_var(ba.to) as i32);
        if m.has_thermal_limit_slacks() {
            rows.push(row);
            cols.push(m.thermal_slack_from_var(ci) as i32);
        }
    }

    // --- Branch flow rows (to-side): rows 2*n_bus + n_br.. ---
    for (ci, ba) in branch_admittances.iter().enumerate() {
        let row = (2 * m.n_bus + n_br + ci) as i32;
        // To-side flow depends on same variables: Va_f, Va_t, Vm_f, Vm_t
        if let Some(va_from) = m.va_var(ba.from) {
            rows.push(row);
            cols.push(va_from as i32);
        }
        if let Some(va_to) = m.va_var(ba.to) {
            rows.push(row);
            cols.push(va_to as i32);
        }
        rows.push(row);
        cols.push(m.vm_var(ba.from) as i32);
        rows.push(row);
        cols.push(m.vm_var(ba.to) as i32);
        if m.has_thermal_limit_slacks() {
            rows.push(row);
            cols.push(m.thermal_slack_to_var(ci) as i32);
        }
    }

    // --- Angle difference constraint rows: rows 2*n_bus + 2*n_br.. ---
    // g_k = Va_from - Va_to
    // dg_k/dVa_from = +1  (if from is not slack)
    // dg_k/dVa_to   = -1  (if to   is not slack)
    let ang_row_base = 2 * m.n_bus + 2 * n_br;
    for (ai, &(br_idx, _, _)) in m.angle_constrained_branches.iter().enumerate() {
        let br = &network.branches[br_idx];
        let f = bus_map[&br.from_bus];
        let t = bus_map[&br.to_bus];
        let row = (ang_row_base + ai) as i32;
        if let Some(vaf) = m.va_var(f) {
            rows.push(row);
            cols.push(vaf as i32);
        }
        if let Some(vat) = m.va_var(t) {
            rows.push(row);
            cols.push(vat as i32);
        }
    }

    // --- Tap ratio columns: τ_k appears in P/Q balance rows for from/to buses ---
    // Each tap variable τ_k = x[tap_var(k)] affects 4 constraint rows:
    //   P-balance row fi: dP[fi]/dτ_k
    //   Q-balance row n_bus+fi: dQ[fi]/dτ_k
    //   P-balance row ti: dP[ti]/dτ_k
    //   Q-balance row n_bus+ti: dQ[ti]/dτ_k
    for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
        let br = &network.branches[br_idx];
        let fi = bus_map[&br.from_bus];
        let ti = bus_map[&br.to_bus];
        let col = m.tap_var(k) as i32;
        rows.push(fi as i32); // P-balance from-bus
        cols.push(col);
        rows.push((m.n_bus + fi) as i32); // Q-balance from-bus
        cols.push(col);
        rows.push(ti as i32); // P-balance to-bus
        cols.push(col);
        rows.push((m.n_bus + ti) as i32); // Q-balance to-bus
        cols.push(col);
    }

    // --- Phase shift columns: θ_s_k appears in P/Q balance rows for from/to buses ---
    for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
        let br = &network.branches[br_idx];
        let fi = bus_map[&br.from_bus];
        let ti = bus_map[&br.to_bus];
        let col = m.ps_var(k) as i32;
        rows.push(fi as i32);
        cols.push(col);
        rows.push((m.n_bus + fi) as i32);
        cols.push(col);
        rows.push(ti as i32);
        cols.push(col);
        rows.push((m.n_bus + ti) as i32);
        cols.push(col);
    }

    // --- Switched shunt columns: b_sw_i appears in Q-balance row for bus k ---
    // Each b_sw_i affects exactly one Q-balance row: n_bus + bus_idx.
    for i in 0..m.n_sw {
        let k = m.switched_shunt_bus_idx[i];
        let col = m.sw_var(i) as i32;
        rows.push((m.n_bus + k) as i32);
        cols.push(col);
    }

    // --- SVC columns: b_svc_i appears in Q-balance row for its bus ---
    for i in 0..m.n_svc {
        let k = m.svc_devices[i].bus_idx;
        rows.push((m.n_bus + k) as i32);
        cols.push(m.svc_var(i) as i32);
    }

    // --- TCSC columns: x_comp_i affects P and Q balance at both from and to buses ---
    for i in 0..m.n_tcsc {
        let tcsc = &m.tcsc_devices[i];
        let col = m.tcsc_var(i) as i32;
        rows.push(tcsc.from_idx as i32); // dP_f/dx_comp
        cols.push(col);
        rows.push((m.n_bus + tcsc.from_idx) as i32); // dQ_f/dx_comp
        cols.push(col);
        rows.push(tcsc.to_idx as i32); // dP_t/dx_comp
        cols.push(col);
        rows.push((m.n_bus + tcsc.to_idx) as i32); // dQ_t/dx_comp
        cols.push(col);
    }

    // --- HVDC Jacobian sparsity ---
    //
    // 1. P-balance rows: P_conv_k column in P-balance row of converter k's AC bus
    //    dP[ac_bus]/dP_conv_k = +1
    // 2. Q-balance rows: Q_conv_k column in Q-balance row of converter k's AC bus
    //    dQ[ac_bus]/dQ_conv_k = +1
    // 3. DC KCL rows (after angle constraints):
    //    - P_conv_k column for each converter at DC bus d
    //    - V_dc_j columns for all DC buses j
    // 4. Current-definition rows:
    //    - P_conv_k, Q_conv_k, Vm_ac_k, I_conv_k
    // 5. DC-control rows:
    //    - Power control: P_conv_k, I_conv_k
    //    - Voltage control: V_dc(dc_bus_k)
    // 6. AC-control rows:
    //    - Reactive-power control: Q_conv_k
    //    - AC-voltage control: Vm(ac_bus_k)
    if let Some(hvdc) = hvdc {
        let dc_kcl_row_base = m.dc_kcl_row_offset as i32;

        // P-balance: each P_conv_k appears in P-balance row of its AC bus
        for k in 0..m.n_conv {
            let ac_bus = m.conv_ac_bus[k];
            rows.push(ac_bus as i32);
            cols.push(m.pconv_var(k) as i32);
        }
        // Q-balance: each Q_conv_k appears in Q-balance row of its AC bus
        for k in 0..m.n_conv {
            let ac_bus = m.conv_ac_bus[k];
            rows.push((m.n_bus + ac_bus) as i32);
            cols.push(m.qconv_var(k) as i32);
        }

        // DC KCL rows
        for d in 0..m.n_dc_bus {
            let row = dc_kcl_row_base + d as i32;

            // P_conv_k columns (one per converter at this DC bus)
            for &_k in &hvdc.dc_bus_conv_map[d] {
                rows.push(row);
                cols.push(m.pconv_var(_k) as i32);
            }

            // I_conv_k columns (one per converter at this DC bus — loss_b/loss_c terms)
            for &k in &hvdc.dc_bus_conv_map[d] {
                rows.push(row);
                cols.push(m.iconv_var(k) as i32);
            }

            // V_dc_j columns (all DC buses)
            for j in 0..m.n_dc_bus {
                rows.push(row);
                cols.push(m.vdc_var(j) as i32);
            }
        }

        // Current-definition rows: P²+Q²-Vm²·I²=0 (one per converter)
        let cur_row_base = m.iconv_eq_row_offset as i32;
        for k in 0..m.n_conv {
            let row = cur_row_base + k as i32;
            let ac_bus = m.conv_ac_bus[k];
            // dh_k/dP_conv_k = 2·P
            rows.push(row);
            cols.push(m.pconv_var(k) as i32);
            // dh_k/dQ_conv_k = 2·Q
            rows.push(row);
            cols.push(m.qconv_var(k) as i32);
            // dh_k/dVm[ac_bus_k] = -2·Vm·I²
            rows.push(row);
            cols.push(m.vm_var(ac_bus) as i32);
            // dh_k/dI_conv_k = -2·Vm²·I
            rows.push(row);
            cols.push(m.iconv_var(k) as i32);
        }

        // DC-control rows.
        let dc_control_row_base = m.dc_control_row_offset as i32;
        for k in 0..m.n_conv {
            let row = dc_control_row_base + k as i32;
            match hvdc.converters[k].dc_control {
                super::hvdc::HvdcDcControlMode::Power => {
                    rows.push(row);
                    cols.push(m.pconv_var(k) as i32);
                    rows.push(row);
                    cols.push(m.iconv_var(k) as i32);
                }
                super::hvdc::HvdcDcControlMode::Voltage => {
                    rows.push(row);
                    cols.push(m.vdc_var(hvdc.converters[k].dc_bus_idx) as i32);
                }
            }
        }

        // AC-control rows.
        let ac_control_row_base = m.ac_control_row_offset as i32;
        for k in 0..m.n_conv {
            let row = ac_control_row_base + k as i32;
            match hvdc.converters[k].ac_control {
                super::hvdc::HvdcAcControlMode::ReactivePower => {
                    rows.push(row);
                    cols.push(m.qconv_var(k) as i32);
                }
                super::hvdc::HvdcAcControlMode::AcVoltage => {
                    rows.push(row);
                    cols.push(m.vm_var(hvdc.converters[k].ac_bus_idx) as i32);
                }
            }
        }
    }

    // --- HVDC point-to-point P Jacobian sparsity (lossless path) ---
    //
    // Each link contributes two entries:
    //   dP[from_bus]/dPg_hvdc[k] = +1   (withdrawal at the from-terminal)
    //   dP[to_bus]/dPg_hvdc[k]   = -1   (injection at the to-terminal)
    //
    // Order matters: `eval_jacobian` must emit values in the exact same
    // order these entries are pushed here. We walk links in `k` order and
    // emit `(from, to)` pairs consistently.
    if let Some(p2p) = hvdc_p2p {
        for k in 0..p2p.links.len() {
            let col = m.hvdc_p2p_var(k) as i32;
            rows.push(m.hvdc_p2p_from_bus_idx[k] as i32);
            cols.push(col);
            rows.push(m.hvdc_p2p_to_bus_idx[k] as i32);
            cols.push(col);
        }
    }

    // --- Storage Jacobian: dP[bus_s]/d_dis[s] = -1, dP[bus_s]/d_ch[s] = +1 ---
    // Both dis[s] and ch[s] appear linearly in P-balance row bus_s only.
    for s in 0..m.n_sto {
        let bus = m.storage_bus_idx[s];
        rows.push(bus as i32);
        cols.push(m.discharge_var(s) as i32);
        rows.push(bus as i32);
        cols.push(m.charge_var(s) as i32);
    }

    // --- Dispatchable-load Jacobian ---
    for k in 0..m.n_dl {
        let bus = m.dispatchable_load_bus_idx[k];
        rows.push(bus as i32);
        cols.push(m.dl_var(k) as i32);
        rows.push((m.n_bus + bus) as i32);
        cols.push(m.dl_q_var(k) as i32);
        if let Some(row) = m.dispatchable_load_pf_rows[k] {
            rows.push(row as i32);
            cols.push(m.dl_var(k) as i32);
            rows.push(row as i32);
            cols.push(m.dl_q_var(k) as i32);
        }
    }

    (rows, cols)
}

// ---------------------------------------------------------------------------
// Hessian sparsity structure (lower triangle)
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
pub(super) fn build_hessian_sparsity(
    mapping: &AcOpfMapping,
    network: &Network,
    ybus: &YBus,
    branch_admittances: &[BranchAdmittance],
    hvdc: Option<&super::hvdc::HvdcNlpData>,
    hvdc_p2p: Option<&super::hvdc::HvdcP2PNlpData>,
) -> (Vec<i32>, Vec<i32>, HashMap<(usize, usize), usize>) {
    let m = mapping;
    let nnz_ybus: usize = (0..m.n_bus).map(|i| ybus.row(i).col_idx.len()).sum();
    let n_tap = m.tap_ctrl_branches.len();
    let n_ps = m.ps_ctrl_branches.len();
    let cap = nnz_ybus * 4
        + branch_admittances.len() * 16
        + m.n_gen
        + n_tap * 3
        + n_ps
        + m.n_sw * 2
        + m.n_svc * 2
        + m.n_tcsc * 6;
    let mut rows = Vec::with_capacity(cap);
    let mut cols = Vec::with_capacity(cap);
    let mut index_map: HashMap<(usize, usize), usize> = HashMap::with_capacity(cap);

    // Insert a lower-triangle entry (row >= col). Returns true if new.
    let mut add_entry = |r: usize, c: usize| {
        let (r, c) = if r >= c { (r, c) } else { (c, r) };
        index_map.entry((r, c)).or_insert_with(|| {
            let idx = rows.len();
            rows.push(r as i32);
            cols.push(c as i32);
            idx
        });
    };

    // Power balance Hessian: sparsity matches Y-bus pattern in (Va,Vm) blocks
    for i in 0..m.n_bus {
        let row = ybus.row(i);
        for &j in row.col_idx {
            // VaVa block
            if let (Some(vai), Some(vaj)) = (m.va_var(i), m.va_var(j)) {
                add_entry(vai, vaj);
            }
            // VmVa cross-block: (Vm_i, Va_j) and (Vm_j, Va_i)
            if let Some(vaj) = m.va_var(j) {
                add_entry(m.vm_var(i), vaj);
            }
            if let Some(vai) = m.va_var(i) {
                add_entry(m.vm_var(j), vai);
            }
            // VmVm block
            add_entry(m.vm_var(i), m.vm_var(j));
        }
    }

    // Branch flow constraints: entries at (f,t) positions (already covered by Y-bus,
    // but ensure all 4×4 combinations are present)
    for ba in branch_admittances {
        let va_vars: [Option<usize>; 2] = [m.va_var(ba.from), m.va_var(ba.to)];
        let vm_vars: [usize; 2] = [m.vm_var(ba.from), m.vm_var(ba.to)];

        // VaVa entries
        for &vai in &va_vars {
            for &vaj in &va_vars {
                if let (Some(a), Some(b)) = (vai, vaj) {
                    add_entry(a, b);
                }
            }
        }
        // VmVm entries
        for &vmi in &vm_vars {
            for &vmj in &vm_vars {
                add_entry(vmi, vmj);
            }
        }
        // VmVa cross entries
        for &vmi in &vm_vars {
            for &vai in &va_vars {
                if let Some(a) = vai {
                    add_entry(vmi, a);
                }
            }
        }
    }

    if m.has_thermal_limit_slacks() {
        for ci in 0..branch_admittances.len() {
            add_entry(m.thermal_slack_from_var(ci), m.thermal_slack_from_var(ci));
            add_entry(m.thermal_slack_to_var(ci), m.thermal_slack_to_var(ci));
        }
    }

    // Objective Hessian: Pg diagonal
    for j in 0..m.n_gen {
        let pg = m.pg_var(j);
        add_entry(pg, pg);
    }

    // Tap ratio Hessian entries.
    //
    // The power-balance Lagrangian gains terms λ_Pf·dPf/dτ + λ_Qf·dQf/dτ + ...
    // The second derivatives of dPf/dτ w.r.t. τ, Vm_f, Vm_t are non-zero:
    //   d²Pf/(dτ²):   nonzero (second derivative of gs/τ² etc.)
    //   d²Pf/(dτ·Vm_f): nonzero (linear in Vf in dPf/dτ expression)
    //   d²Pf/(dτ·Vm_t): nonzero (linear in Vt in dPf/dτ expression)
    // Cross-terms with Va are zero (angle enters via fixed cos/sin(Va_f-Va_t) in dPf/dτ,
    // but the derivative of dPf/dτ w.r.t. Va is nonzero too — add Va cross terms).
    {
        let bus_map = network.bus_index_map();
        for (k, &(br_idx, _, _)) in m.tap_ctrl_branches.iter().enumerate() {
            let br = &network.branches[br_idx];
            let fi = bus_map[&br.from_bus];
            let ti = bus_map[&br.to_bus];
            let tau_var = m.tap_var(k);

            // (τ_k, τ_k) diagonal
            add_entry(tau_var, tau_var);
            // (Vm_f, τ_k) and (Vm_t, τ_k) cross-terms: dPf/dτ is linear in Vf, Vt
            add_entry(m.vm_var(fi), tau_var);
            add_entry(m.vm_var(ti), tau_var);
            // (Va_f, τ_k) and (Va_t, τ_k) cross-terms: dPf/dτ contains Vf·Vt·(...) which
            // has d/dVa contribution through cos(Va_f-Va_t), sin(Va_f-Va_t)
            if let Some(va_f) = m.va_var(fi) {
                add_entry(va_f, tau_var);
            }
            if let Some(va_t) = m.va_var(ti) {
                add_entry(va_t, tau_var);
            }
        }

        // Phase shift Hessian entries.
        // dPf/dθ_s is linear in Vf·Vt and contains cos/sin(θ_ft).
        // Second derivatives:
        //   d²Pf/(dθ_s²): nonzero (second derivative of trig functions of θ_s)
        //   d²Pf/(dθ_s·Vm_f): nonzero
        //   d²Pf/(dθ_s·Vm_t): nonzero
        //   d²Pf/(dθ_s·Va_f): nonzero (θ_ft = Va_f - Va_t)
        //   d²Pf/(dθ_s·Va_t): nonzero
        for (k, &(br_idx, _, _)) in m.ps_ctrl_branches.iter().enumerate() {
            let br = &network.branches[br_idx];
            let fi = bus_map[&br.from_bus];
            let ti = bus_map[&br.to_bus];
            let ps_var = m.ps_var(k);

            add_entry(ps_var, ps_var);
            add_entry(m.vm_var(fi), ps_var);
            add_entry(m.vm_var(ti), ps_var);
            if let Some(va_f) = m.va_var(fi) {
                add_entry(va_f, ps_var);
            }
            if let Some(va_t) = m.va_var(ti) {
                add_entry(va_t, ps_var);
            }
        }

        // Switched shunt Hessian entries.
        // dg[n_bus+k]/db_sw_i = -Vm[k]²
        // d²L/(db_sw_i²) = 0 (linear — but add diagonal for Ipopt structure)
        // d²L/(db_sw_i · Vm[k]) = λ_Q[k] * (-2·Vm[k])
        for i in 0..m.n_sw {
            let k = m.switched_shunt_bus_idx[i];
            let sw_var = m.sw_var(i);
            let vm_k = m.vm_var(k);
            add_entry(sw_var, sw_var); // diagonal (value = 0, but structure required)
            add_entry(vm_k, sw_var); // (Vm[k], b_sw_i) cross-term
        }

        // SVC Hessian entries: same pattern as switched shunts.
        // (b_svc, b_svc) diagonal + (Vm[k], b_svc) cross-term
        for i in 0..m.n_svc {
            let svc_v = m.svc_var(i);
            let k = m.svc_devices[i].bus_idx;
            let vm_k = m.vm_var(k);
            add_entry(svc_v, svc_v); // diagonal (value=0, for Ipopt structure)
            add_entry(vm_k, svc_v); // (Vm[k], b_svc) cross-term
        }

        // TCSC Hessian entries: diagonal + Vm and Va cross-terms
        for i in 0..m.n_tcsc {
            let xc_v = m.tcsc_var(i);
            let tcsc = &m.tcsc_devices[i];
            let vm_f = m.vm_var(tcsc.from_idx);
            let vm_t = m.vm_var(tcsc.to_idx);
            add_entry(xc_v, xc_v); // (x_comp, x_comp) diagonal
            add_entry(vm_f, xc_v); // (Vm_f, x_comp)
            add_entry(vm_t, xc_v); // (Vm_t, x_comp)
            if let Some(va_f) = m.va_var(tcsc.from_idx) {
                add_entry(va_f, xc_v);
            }
            if let Some(va_t) = m.va_var(tcsc.to_idx) {
                add_entry(va_t, xc_v);
            }
        }

        // HVDC DC KCL Hessian entries.
        // DC KCL constraint: Σ (P_conv - loss_a - loss_b·I - loss_c·I²) + Σ G_dc·Vd·Vj
        //   V_dc terms: d²/d(V_dc_d)² = 2*G_dc(d,d), d²/d(V_dc_d)d(V_dc_j) = G_dc(d,j)
        //   I_conv terms: d²/d(I_conv_k)² = -2·loss_c_k
        if let Some(hvdc) = hvdc {
            for d in 0..m.n_dc_bus {
                let vdc_d = m.vdc_var(d);
                add_entry(vdc_d, vdc_d); // diagonal
                for j in 0..m.n_dc_bus {
                    if j != d && hvdc.g_dc[d][j].abs() > 1e-30 {
                        add_entry(m.vdc_var(d), m.vdc_var(j)); // off-diagonal
                    }
                }
                // I_conv diagonal for loss_c quadratic term
                for &k in &hvdc.dc_bus_conv_map[d] {
                    add_entry(m.iconv_var(k), m.iconv_var(k));
                }
            }

            // Current-definition Hessian entries: h_k = P²+Q²-Vm²·I²
            //   d²h/dP² = 2, d²h/dQ² = 2
            //   d²h/dVm² = -2·I², d²h/dI² = -2·Vm²
            //   d²h/(dVm·dI) = -4·Vm·I
            for k in 0..m.n_conv {
                let ac_bus = m.conv_ac_bus[k];
                let pvar = m.pconv_var(k);
                let qvar = m.qconv_var(k);
                let vmvar = m.vm_var(ac_bus);
                let ivar = m.iconv_var(k);
                add_entry(pvar, pvar); // (P, P) diagonal
                add_entry(qvar, qvar); // (Q, Q) diagonal
                add_entry(vmvar, vmvar); // (Vm, Vm) — may already exist, add_entry deduplicates
                add_entry(ivar, ivar); // (I, I) diagonal — may already exist from DC KCL
                add_entry(vmvar, ivar); // (Vm, I) cross-term
            }

            // DC power-control Hessian entries: only I_conv² from -loss_c·I².
            for k in 0..m.n_conv {
                if hvdc.converters[k].dc_control == super::hvdc::HvdcDcControlMode::Power {
                    add_entry(m.iconv_var(k), m.iconv_var(k));
                }
            }
        }

        // HVDC point-to-point Hessian entries: the split-loss bus
        // balance contribution `g[from] += Pg + 0.5*c*Pg²` and
        // `g[to] += -Pg + 0.5*c*Pg²` has a nonzero second derivative
        // `d²/dPg² = c_pu` on each constraint row. The Lagrangian
        // Hessian contribution at `(hvdc_var, hvdc_var)` is
        // `c_pu * (λ_from + λ_to)`. Only add the diagonal entry when
        // at least one link is lossy (`c_pu > 0`); lossless links
        // contribute nothing.
        if let Some(p2p) = hvdc_p2p {
            for (k, link) in p2p.links.iter().enumerate() {
                if link.loss_c_pu.abs() > 1e-20 {
                    let v = m.hvdc_p2p_var(k);
                    add_entry(v, v);
                }
            }
        }
    }

    (rows, cols, index_map)
}

// ---------------------------------------------------------------------------
// Initial point construction
// ---------------------------------------------------------------------------

pub(super) fn build_initial_point(
    network: &Network,
    mapping: &AcOpfMapping,
    branch_admittances: &[BranchAdmittance],
    regulated_bus_vm_targets: &[Option<f64>],
    enforce_regulated_bus_vm_targets: bool,
    warm_start: Option<&WarmStart>,
    dc_opf_angles: Option<&[f64]>,
    hvdc: Option<&super::hvdc::HvdcNlpData>,
    hvdc_p2p: Option<&super::hvdc::HvdcP2PNlpData>,
) -> Vec<f64> {
    let m = mapping;
    let base = network.base_mva;
    let mut x0 = vec![0.0; m.n_var];

    if let Some(prior) = warm_start {
        // Warm-start: initialise from a prior AC operating point.
        let prior_voltage_angles_rad =
            re_reference_warm_start_angles(network, mapping, &prior.voltage_angle_rad);

        // Va for non-slack buses
        #[allow(clippy::needless_range_loop)]
        for i in 0..m.n_bus {
            if let Some(idx) = m.va_var(i) {
                x0[idx] = prior_voltage_angles_rad[i];
            }
        }
        // Vm for all buses
        for i in 0..m.n_bus {
            let vm_seed = if i < prior.voltage_magnitude_pu.len() {
                prior.voltage_magnitude_pu[i]
            } else {
                1.0
            };
            let bus = &network.buses[i];
            let regulated_target = regulated_bus_vm_targets.get(i).copied().flatten();
            let vm_init = if enforce_regulated_bus_vm_targets {
                if let Some(target_vm) = regulated_target {
                    vm_seed.clamp(target_vm, target_vm)
                } else {
                    vm_seed.clamp(bus.voltage_min_pu, bus.voltage_max_pu)
                }
            } else {
                vm_seed.clamp(bus.voltage_min_pu, bus.voltage_max_pu)
            };
            x0[m.vm_var(i)] = vm_init;
        }
        // Pg from prior dispatch, clamped to bounds
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            let pg_seed_pu = if j < prior.pg.len() {
                prior.pg[j]
            } else {
                g.p / base
            };
            let pg_pu = if g.is_storage() {
                0.0
            } else {
                pg_seed_pu.clamp(g.pmin / base, g.pmax / base)
            };
            x0[m.pg_var(j)] = pg_pu;
        }
        // Qg from prior dispatch, clamped to bounds
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
            let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
            let qg_seed_pu = if j < prior.qg.len() {
                prior.qg[j]
            } else {
                g.q / base
            };
            let qg_pu = qg_seed_pu.clamp(qmin / base, qmax / base);
            x0[m.qg_var(j)] = qg_pu;
        }
        for (k, dl) in network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|dl| dl.in_service)
            .enumerate()
        {
            let p_seed_pu = prior
                .dispatchable_load_p
                .get(k)
                .copied()
                .unwrap_or(dl.p_sched_pu);
            let q_seed_pu = prior
                .dispatchable_load_q
                .get(k)
                .copied()
                .unwrap_or(dl.q_sched_pu);
            x0[m.dl_var(k)] = p_seed_pu.clamp(dl.p_min_pu, dl.p_max_pu);
            x0[m.dl_q_var(k)] = q_seed_pu.clamp(dl.q_min_pu, dl.q_max_pu);
        }
    } else {
        // Default: DC warm-start for angles, case data for Vm/Pg/Qg.

        // Use DC-OPF angles when provided; otherwise fall back to DC power flow.
        let dc_angles_buf: Vec<f64>;
        let dc_angles: &[f64] = if let Some(angles) = dc_opf_angles {
            angles
        } else {
            dc_angles_buf = match surge_dc::solve_dc(network) {
                Ok(r) => r.theta,
                Err(e) => {
                    tracing::warn!(
                        "DC power flow warm-start failed ({}); \
                         AC-OPF will start from flat (zero-angle) initial point",
                        e
                    );
                    vec![0.0; m.n_bus]
                }
            };
            &dc_angles_buf
        };

        for (i, &angle) in dc_angles.iter().enumerate().take(m.n_bus) {
            if let Some(idx) = m.va_var(i) {
                x0[idx] = angle;
            }
        }

        // Vm from generator setpoints or case data
        let bus_map = network.bus_index_map();
        let mut vm_init = vec![1.0; m.n_bus];
        for bus in &network.buses {
            let idx = bus_map[&bus.number];
            vm_init[idx] = bus.voltage_magnitude_pu;
        }
        for g in &network.generators {
            if g.in_service {
                let idx = bus_map[&g.bus];
                vm_init[idx] = g.voltage_setpoint_pu;
            }
        }
        for i in 0..m.n_bus {
            let bus = &network.buses[i];
            let value = vm_init[i];
            let regulated_target = regulated_bus_vm_targets.get(i).copied().flatten();
            let vm_bounded = if enforce_regulated_bus_vm_targets {
                if let Some(target_vm) = regulated_target {
                    value.clamp(target_vm, target_vm)
                } else {
                    value.clamp(bus.voltage_min_pu, bus.voltage_max_pu)
                }
            } else {
                value.clamp(bus.voltage_min_pu, bus.voltage_max_pu)
            };
            x0[m.vm_var(i)] = vm_bounded;
        }

        // Pg from case data, clamped to bounds
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            let pg_pu = if g.is_storage() {
                0.0
            } else {
                (g.p / base).clamp(g.pmin / base, g.pmax / base)
            };
            x0[m.pg_var(j)] = pg_pu;
        }

        // Qg from case data, clamped to bounds
        for (j, &gi) in m.gen_indices.iter().enumerate() {
            let g = &network.generators[gi];
            let qmin = if g.qmin.abs() > 1e10 { -9999.0 } else { g.qmin };
            let qmax = if g.qmax.abs() > 1e10 { 9999.0 } else { g.qmax };
            let qg_pu = (g.q / base).clamp(qmin / base, qmax / base);
            x0[m.qg_var(j)] = qg_pu;
        }
    }

    // Tap ratio initial values: from case-data tap, clamped to [tap_min, tap_max].
    // MATPOWER convention: tap = 0 means no transformer (use 1.0 as initial).
    for (k, &(br_idx, tau_min, tau_max)) in m.tap_ctrl_branches.iter().enumerate() {
        let tap0 = network.branches[br_idx].tap;
        let tau_init = if tap0.abs() < 1e-10 { 1.0 } else { tap0 };
        x0[m.tap_var(k)] = tau_init.clamp(tau_min, tau_max);
    }

    // Phase shift initial values: from case-data shift (radians),
    // clamped to [phase_min_rad, phase_max_rad].
    for (k, &(br_idx, ps_min_rad, ps_max_rad)) in m.ps_ctrl_branches.iter().enumerate() {
        let shift_init_rad = network.branches[br_idx].phase_shift_rad;
        x0[m.ps_var(k)] = shift_init_rad.clamp(ps_min_rad, ps_max_rad);
    }

    // Switched shunt initial susceptance: from case-data b_init_pu,
    // clamped to [b_min_pu, b_max_pu].
    for i in 0..m.n_sw {
        let shunt = &network.controls.switched_shunts_opf[i];
        x0[m.sw_var(i)] = shunt.b_init_pu.clamp(shunt.b_min_pu, shunt.b_max_pu);
    }

    // SVC initial susceptance.
    for i in 0..m.n_svc {
        x0[m.svc_var(i)] = m.svc_devices[i].b_init;
    }
    // TCSC initial compensation.
    for i in 0..m.n_tcsc {
        x0[m.tcsc_var(i)] = m.tcsc_devices[i].x_comp_init;
    }

    // HVDC converter initial values.
    // P_conv from p_dc_set_pu, Q_conv = 0, V_dc from v_dc_set,
    // I_conv = sqrt(P² + Q²) / Vm (from current definition).
    if let Some(h) = hvdc {
        for k in 0..m.n_conv {
            let c = &h.converters[k];
            let vm_ac = x0[m.vm_var(c.ac_bus_idx)].max(0.5);
            let p_seed = match c.dc_control {
                super::hvdc::HvdcDcControlMode::Power => c.p_dc_set_pu,
                super::hvdc::HvdcDcControlMode::Voltage => 0.0,
            };
            let q_seed = match c.ac_control {
                super::hvdc::HvdcAcControlMode::ReactivePower => c.q_ac_set_pu,
                super::hvdc::HvdcAcControlMode::AcVoltage => 0.0,
            };
            let p0 = p_seed.clamp(c.p_min_pu, c.p_max_pu);
            let q0 = q_seed.clamp(c.q_min_pu, c.q_max_pu);
            x0[m.pconv_var(k)] = p0;
            x0[m.qconv_var(k)] = q0;
            // I_conv initial: |S| / Vm at AC bus
            let s_mag = (p0 * p0 + q0 * q0).sqrt();
            let i0 = s_mag / vm_ac;
            x0[m.iconv_var(k)] = if c.i_max_pu.is_finite() && c.i_max_pu > 0.0 {
                i0.min(c.i_max_pu)
            } else {
                i0
            };
        }
        for d in 0..m.n_dc_bus {
            let (vmin, vmax) = h.vdc_bounds[d];
            let v_set = h
                .converters
                .iter()
                .find(|c| c.dc_bus_idx == d)
                .map(|c| c.voltage_dc_setpoint_pu)
                .unwrap_or(1.0);
            x0[m.vdc_var(d)] = v_set.clamp(vmin, vmax);
        }
    }

    // HVDC point-to-point initial values (seeded from `p_warm_start_pu`
    // which the builder clamped to `[p_min_pu, p_max_pu]` at construction
    // time). The warm start source is `scheduled_setpoint` on each link,
    // which in turn is populated from the DC SCED target before the AC
    // OPF call.
    if let Some(p2p) = hvdc_p2p {
        for (k, link) in p2p.links.iter().enumerate() {
            x0[m.hvdc_p2p_var(k)] = link.p_warm_start_pu;
        }
    }

    if warm_start.is_none() {
        for (k, dl) in network
            .market_data
            .dispatchable_loads
            .iter()
            .filter(|dl| dl.in_service)
            .enumerate()
        {
            x0[m.dl_var(k)] = dl.p_sched_pu.clamp(dl.p_min_pu, dl.p_max_pu);
            x0[m.dl_q_var(k)] = dl.q_sched_pu.clamp(dl.q_min_pu, dl.q_max_pu);
        }
    }

    if m.has_p_bus_balance_slacks() {
        for i in 0..m.n_bus {
            x0[m.p_balance_slack_pos_var(i)] = 0.0;
            x0[m.p_balance_slack_neg_var(i)] = 0.0;
        }
    }
    if m.has_q_bus_balance_slacks() {
        for i in 0..m.n_bus {
            x0[m.q_balance_slack_pos_var(i)] = 0.0;
            x0[m.q_balance_slack_neg_var(i)] = 0.0;
        }
    }
    if m.has_thermal_limit_slacks() {
        let (va, vm, _, _) = m.extract_voltages_and_dispatch(&x0);
        let vm = vm.to_vec();
        for (ci, ba) in branch_admittances.iter().enumerate() {
            let (pf, qf) = branch_flow_from(ba, &vm, &va);
            let overflow_from = (pf.hypot(qf) - ba.s_max_pu()).max(0.0);
            x0[m.thermal_slack_from_var(ci)] = overflow_from;

            let (pt, qt) = branch_flow_to(ba, &vm, &va);
            let overflow_to = (pt.hypot(qt) - ba.s_max_pu()).max(0.0);
            x0[m.thermal_slack_to_var(ci)] = overflow_to;
        }
    }

    x0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ac::mapping::AcOpfMapping;
    use crate::common::context::OpfNetworkContext;
    use surge_network::Network;
    use surge_network::market::CostCurve;
    use surge_network::network::{Branch, Bus, BusType, Generator, Load};

    #[test]
    fn build_initial_point_references_warm_start_angles_to_current_slack() {
        let mut net = Network::new("warm-start-reference");
        net.buses.push(Bus::new(1, BusType::PQ, 138.0));
        net.buses.push(Bus::new(2, BusType::Slack, 138.0));
        net.buses.push(Bus::new(3, BusType::PQ, 138.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.0));
        net.loads.push(Load::new(1, 20.0, 5.0));
        net.loads.push(Load::new(3, 25.0, 8.0));

        let mut slack_gen = Generator::new(2, 50.0, 1.01);
        slack_gen.pmax = 100.0;
        slack_gen.qmin = -50.0;
        slack_gen.qmax = 50.0;
        slack_gen.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 1.0, 0.0],
        });
        net.generators.push(slack_gen);

        let context = OpfNetworkContext::for_ac(&net).expect("AC context");
        let mapping = AcOpfMapping::new(
            &context,
            vec![],
            false,
            false,
            false,
            false,
            false,
            false,
            None,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        )
        .expect("mapping");

        let warm_start = WarmStart {
            voltage_magnitude_pu: vec![1.0, 1.01, 0.99],
            voltage_angle_rad: vec![0.12, 0.27, -0.08],
            pg: vec![0.5],
            qg: vec![0.1],
            dispatchable_load_p: vec![],
            dispatchable_load_q: vec![],
        };

        let x0 = build_initial_point(
            &net,
            &mapping,
            &[],
            &[],
            true,
            Some(&warm_start),
            None,
            None,
            None,
        );
        let bus1_va = x0[mapping.va_var(0).expect("bus 1 should have a Va variable")];
        let bus3_va = x0[mapping.va_var(2).expect("bus 3 should have a Va variable")];

        assert!((bus1_va - (0.12 - 0.27)).abs() <= 1e-12);
        assert!((bus3_va - (-0.08 - 0.27)).abs() <= 1e-12);
        assert!(
            mapping.va_var(1).is_none(),
            "slack bus should have no Va variable"
        );
    }

    #[test]
    fn build_initial_point_seeds_thermal_slacks_from_overload() {
        let mut net = Network::new("thermal-slack-seed");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));

        let mut branch = Branch::new_line(1, 2, 0.01, 0.1, 0.0);
        branch.rating_a_mva = 10.0;
        net.branches.push(branch);

        let mut slack_gen = Generator::new(1, 50.0, 1.0);
        slack_gen.pmax = 200.0;
        slack_gen.qmin = -200.0;
        slack_gen.qmax = 200.0;
        slack_gen.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 1.0, 0.0],
        });
        net.generators.push(slack_gen);
        net.loads.push(Load::new(2, 40.0, 0.0));

        let context = OpfNetworkContext::for_ac(&net).expect("AC context");
        let mapping = AcOpfMapping::new(
            &context,
            vec![0],
            false,
            false,
            false,
            false,
            false,
            false,
            None,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
            false,
            false,
            false,
            true,
            false,
            false,
            false,
        )
        .expect("mapping");
        let bus_map = net.bus_index_map();
        let branch_admittances =
            super::super::types::build_branch_admittances(&net, &[0], &bus_map);

        let x0 = build_initial_point(
            &net,
            &mapping,
            &branch_admittances,
            &[],
            true,
            None,
            None,
            None,
            None,
        );
        let (va, vm, _, _) = mapping.extract_voltages_and_dispatch(&x0);
        let (pf, qf) = branch_flow_from(&branch_admittances[0], vm, &va);
        let (pt, qt) = branch_flow_to(&branch_admittances[0], vm, &va);
        let expected_from = (pf.hypot(qf) - branch_admittances[0].s_max_pu()).max(0.0);
        let expected_to = (pt.hypot(qt) - branch_admittances[0].s_max_pu()).max(0.0);

        assert!(
            (x0[mapping.thermal_slack_from_var(0)] - expected_from).abs() <= 1e-12,
            "from-side thermal slack should seed to the overload amount"
        );
        assert!(
            (x0[mapping.thermal_slack_to_var(0)] - expected_to).abs() <= 1e-12,
            "to-side thermal slack should seed to the overload amount"
        );
    }

    #[test]
    fn build_initial_point_clamps_regulated_bus_vm_to_target() {
        let mut net = Network::new("regulated-vm-seed");
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.buses.push(Bus::new(2, BusType::PQ, 138.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.loads.push(Load::new(2, 20.0, 5.0));

        let mut slack_gen = Generator::new(1, 30.0, 1.02);
        slack_gen.pmax = 100.0;
        slack_gen.qmin = -50.0;
        slack_gen.qmax = 50.0;
        slack_gen.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 1.0, 0.0],
        });
        net.generators.push(slack_gen);

        let context = OpfNetworkContext::for_ac(&net).expect("AC context");
        let mapping = AcOpfMapping::new(
            &context,
            vec![],
            false,
            false,
            false,
            false,
            false,
            false,
            None,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        )
        .expect("mapping");
        let regulated_targets = vec![Some(1.02), None];
        let warm_start = WarmStart {
            voltage_magnitude_pu: vec![0.95, 0.99],
            voltage_angle_rad: vec![0.15, -0.02],
            pg: vec![0.3],
            qg: vec![0.0],
            dispatchable_load_p: vec![],
            dispatchable_load_q: vec![],
        };

        let x0 = build_initial_point(
            &net,
            &mapping,
            &[],
            &regulated_targets,
            true,
            Some(&warm_start),
            None,
            None,
            None,
        );

        assert!((x0[mapping.vm_var(0)] - 1.02).abs() <= 1e-12);
        assert!((x0[mapping.vm_var(1)] - 0.99).abs() <= 1e-12);

        // When `enforce_regulated_bus_vm_targets = false`, the regulated bus
        // keeps its `[vmin, vmax]` bounds and the warm-start vm is honored
        // (clamped to those bounds), instead of being overwritten by the
        // regulator setpoint.
        let x0_relaxed = build_initial_point(
            &net,
            &mapping,
            &[],
            &regulated_targets,
            false,
            Some(&warm_start),
            None,
            None,
            None,
        );
        assert!((x0_relaxed[mapping.vm_var(0)] - 0.95).abs() <= 1e-12);
        assert!((x0_relaxed[mapping.vm_var(1)] - 0.99).abs() <= 1e-12);
    }
}
