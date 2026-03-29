// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF NLP problem construction.
//!
//! Contains [`AcOpfProblem`] (the NLP formulation) and [`HessDirectIdx`]
//! (direct-indexed Hessian positions for hot-path elimination of HashMap overhead).

use std::collections::HashMap;

use surge_ac::matrix::ybus::{YBus, build_ybus};
use surge_network::Network;

use crate::common::context::OpfNetworkContext;

use super::mapping::AcOpfMapping;
use super::sparsity::{build_hessian_sparsity, build_initial_point, build_jacobian_sparsity};
use super::types::{
    AcOpfError, AcOpfOptions, AcOpfRunContext, BranchAdmittance, FgBranchEntry, FgConstraintData,
    build_branch_admittances,
};

pub(super) const HESS_SKIP: usize = usize::MAX;

/// Pre-computed Hessian positions for the power balance and branch flow sections.
///
/// In `eval_hessian`, the power balance loop over Y-bus NNZ generates ~90k `add()` calls
/// per evaluation (2000-bus network). Each `add()` does a HashMap lookup costing ~20ns.
/// Over 200 Ipopt iterations that's ~360ms of pure hash overhead. This struct replaces
/// the HashMap with O(1) array indexing for the two hottest sections.
pub(super) struct HessDirectIdx {
    /// Pg diagonal position per generator j.
    pub(super) pg_diag: Vec<usize>,
    /// Vm diagonal position per bus i.
    pub(super) vm_diag: Vec<usize>,
    /// Per Y-bus NNZ at flat index [ybus_row_offsets[i] + k]: 8 positions for
    /// power balance cross-terms from neighbor j.
    ///
    /// Entry order:
    ///   [0] (Va_j, Va_j)     — HESS_SKIP if j is slack
    ///   [1] (Va_i, Va_j)     — HESS_SKIP if i or j is slack
    ///   [2] (Va_i, Va_i)     — HESS_SKIP if i is slack
    ///   [3] (Vm_i, Va_j)     — HESS_SKIP if j is slack
    ///   [4] (Vm_j, Va_j)     — HESS_SKIP if j is slack
    ///   [5] (Vm_j, Va_i)     — HESS_SKIP if i is slack
    ///   [6] (Vm_i, Va_i)     — HESS_SKIP if i is slack
    ///   [7] (Vm_i, Vm_j)     — always valid
    pub(super) ybus_pb: Vec<[usize; 8]>,
    /// Branch flow from-side: 10 lower-triangle positions per constrained branch.
    /// Indexed by 4×4 lower triangle: (a,b) for a >= b, flattened as
    ///   [(0,0), (1,0), (1,1), (2,0), (2,1), (2,2), (3,0), (3,1), (3,2), (3,3)]
    /// where variable order is [θ_from, θ_to, Vm_from, Vm_to].
    pub(super) branch_from: Vec<[usize; 10]>,
    /// Branch flow to-side: same layout.
    pub(super) branch_to: Vec<[usize; 10]>,
}

/// Build the direct-index Hessian lookup from the HashMap created by `build_hessian_sparsity`.
pub(super) fn build_hess_direct_idx(
    mapping: &AcOpfMapping,
    ybus: &YBus,
    ybus_row_offsets: &[usize],
    branch_admittances: &[BranchAdmittance],
    hess_map: &HashMap<(usize, usize), usize>,
) -> HessDirectIdx {
    let m = mapping;

    // Helper: resolve (r, c) to lower-triangle Hessian position, or HESS_SKIP.
    let lookup = |r: usize, c: usize| -> usize {
        let (r, c) = if r >= c { (r, c) } else { (c, r) };
        hess_map.get(&(r, c)).copied().unwrap_or(HESS_SKIP)
    };

    // Pg diagonal
    let pg_diag: Vec<usize> = (0..m.n_gen)
        .map(|j| {
            let pg = m.pg_var(j);
            lookup(pg, pg)
        })
        .collect();

    // Vm diagonal
    let vm_diag: Vec<usize> = (0..m.n_bus)
        .map(|i| {
            let vmi = m.vm_var(i);
            lookup(vmi, vmi)
        })
        .collect();

    // Y-bus power balance cross-terms
    let total_nnz = *ybus_row_offsets.last().unwrap_or(&0);
    let mut ybus_pb = vec![[HESS_SKIP; 8]; total_nnz];
    for (i, &base) in ybus_row_offsets.iter().enumerate().take(m.n_bus) {
        let row = ybus.row(i);
        let vmi = m.vm_var(i);
        let vai_opt = m.va_var(i);
        for (k, &j) in row.col_idx.iter().enumerate() {
            if j == i {
                continue; // diagonal entries not used in the off-diagonal loop
            }
            let vaj_opt = m.va_var(j);
            let vmj = m.vm_var(j);
            let idx = base + k;

            ybus_pb[idx][0] = vaj_opt.map_or(HESS_SKIP, |vaj| lookup(vaj, vaj));
            ybus_pb[idx][1] = match (vai_opt, vaj_opt) {
                (Some(vai), Some(vaj)) => lookup(vai, vaj),
                _ => HESS_SKIP,
            };
            ybus_pb[idx][2] = vai_opt.map_or(HESS_SKIP, |vai| lookup(vai, vai));
            ybus_pb[idx][3] = vaj_opt.map_or(HESS_SKIP, |vaj| lookup(vmi, vaj));
            ybus_pb[idx][4] = vaj_opt.map_or(HESS_SKIP, |vaj| lookup(vmj, vaj));
            ybus_pb[idx][5] = vai_opt.map_or(HESS_SKIP, |vai| lookup(vmj, vai));
            ybus_pb[idx][6] = vai_opt.map_or(HESS_SKIP, |vai| lookup(vmi, vai));
            ybus_pb[idx][7] = lookup(vmi, vmj);
        }
    }

    // Branch flow positions (4×4 lower triangle)
    let resolve_branch = |ba: &BranchAdmittance| -> [usize; 10] {
        let vars: [Option<usize>; 4] = [
            m.va_var(ba.from),
            m.va_var(ba.to),
            Some(m.vm_var(ba.from)),
            Some(m.vm_var(ba.to)),
        ];
        let mut pos = [HESS_SKIP; 10];
        let mut flat = 0;
        for a_idx in 0..4 {
            for b_idx in 0..=a_idx {
                if let (Some(va), Some(vb)) = (vars[a_idx], vars[b_idx]) {
                    pos[flat] = lookup(va, vb);
                }
                flat += 1;
            }
        }
        pos
    };

    let branch_from: Vec<[usize; 10]> = branch_admittances.iter().map(resolve_branch).collect();
    let branch_to: Vec<[usize; 10]> = branch_admittances.iter().map(resolve_branch).collect();

    HessDirectIdx {
        pg_diag,
        vm_diag,
        ybus_pb,
        branch_from,
        branch_to,
    }
}

// ---------------------------------------------------------------------------
// AC-OPF NLP Problem
// ---------------------------------------------------------------------------

/// AC-OPF problem formulation implementing the NlpProblem trait.
#[allow(dead_code)]
pub(super) struct AcOpfProblem<'a> {
    pub(super) network: &'a Network,
    pub(super) ybus: YBus,
    pub(super) mapping: AcOpfMapping,
    pub(super) branch_admittances: Vec<BranchAdmittance>,
    pub(super) base_mva: f64,
    /// Cached bus number → internal index map (avoids rebuilding HashMap in hot callbacks).
    pub(super) bus_map: HashMap<u32, usize>,
    /// Cumulative Y-bus NNZ offsets per row (for sin/cos cache indexing).
    pub(super) ybus_row_offsets: Vec<usize>,
    /// Cached Jacobian sparsity structure.
    pub(super) jac_rows: Vec<i32>,
    pub(super) jac_cols: Vec<i32>,
    /// Cached Hessian sparsity structure (lower triangle).
    pub(super) hess_rows: Vec<i32>,
    pub(super) hess_cols: Vec<i32>,
    /// Map (row_var, col_var) → index in Hessian values array (lower triangle: row >= col).
    /// Used only for tap/phase/shunt/HVDC Hessian entries (low frequency).
    pub(super) hess_map: HashMap<(usize, usize), usize>,
    /// Direct-index Hessian positions for power balance and branch flow (hot path).
    pub(super) hess_idx: HessDirectIdx,
    /// Initial point from DC warm-start.
    pub(super) x0: Vec<f64>,
    /// Whether hard thermal limit constraints are active in the NLP.
    ///
    /// When `true`, `branch_admittances` contains constrained branches and the hard
    /// NLP constraints enforce thermal limits; the thermal penalty gradient is omitted
    /// to avoid conflicting with the constraint multipliers (Ipopt handles it).
    /// When `false`, there are no hard thermal constraints and the penalty gradient
    /// provides the only enforcement signal.
    pub(super) enforce_thermal_limits: bool,
    /// Whether switched shunt susceptance banks are NLP variables.
    pub(super) optimize_switched_shunts: bool,
    /// Whether SVC/STATCOM susceptance is an NLP variable.
    pub(super) optimize_svc: bool,
    /// Whether TCSC compensating reactance is an NLP variable.
    pub(super) optimize_tcsc: bool,
    /// HVDC NLP data (when joint AC-DC NLP is active).
    pub(super) hvdc: Option<super::hvdc::HvdcNlpData>,
    /// Storage generator global indices co-optimized as NLP variables (CostMinimization mode).
    pub(super) storage_gen_indices: Vec<usize>,
    /// State of charge (MWh) for each storage generator at interval start (indexed by s).
    pub(super) storage_soc_mwh: Vec<f64>,
    /// Interval duration (hours) for SoC-derived bounds.
    pub(super) dt_hours: f64,
    /// Benders cuts from AC-SCOPF (linear constraints α^T Pg ≤ rhs in per-unit).
    pub(super) cuts: Vec<super::sensitivity::BendersCut>,
    /// Linearized D-curve constraints built from generators' `pq_curve` fields.
    pub(super) pq_constraints: Vec<super::pq_curve::PqConstraint>,
    /// Pre-computed flowgate constraint data (one per active flowgate).
    pub(super) fg_data: Vec<FgConstraintData>,
    /// Pre-computed interface constraint data (one per active interface).
    pub(super) iface_data: Vec<FgConstraintData>,
    /// Per-bus real power demand (MW), computed from loads and power injections.
    pub(super) bus_pd_mw: Vec<f64>,
    /// Per-bus reactive power demand (MVAr), computed from loads and power injections.
    pub(super) bus_qd_mvar: Vec<f64>,
}

impl<'a> AcOpfProblem<'a> {
    pub(super) fn new(
        network: &'a Network,
        options: &AcOpfOptions,
        context: &AcOpfRunContext,
        dc_opf_angles: Option<&[f64]>,
        constrained_branches_override: Option<Vec<usize>>,
    ) -> Result<Self, AcOpfError> {
        let net_context = OpfNetworkContext::for_ac(network)?;
        let bus_map = net_context.bus_map.clone();

        // Identify constrained branches (caller may provide an explicit subset).
        let constrained_branches: Vec<usize> = if let Some(ov) = constrained_branches_override {
            ov
        } else if options.enforce_thermal_limits {
            network
                .branches
                .iter()
                .enumerate()
                .filter(|(_, br)| br.in_service && br.rating_a_mva >= options.min_rate_a)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };

        // Build HVDC NLP data if the network has DC network converter data
        // and the caller hasn't explicitly disabled HVDC.
        let hvdc_data: Option<super::hvdc::HvdcNlpData> = match options.include_hvdc {
            Some(false) => None,
            _ => super::hvdc::build_hvdc_nlp_data(network)?,
        };

        // Extract CostMinimization storage generators for native NLP co-optimization.
        // SelfSchedule and OfferCurve units must be pre-dispatched by the caller.
        let mut storage_gen_indices: Vec<usize> = Vec::new();
        let mut storage_soc_mwh: Vec<f64> = Vec::new();
        for (gi, g) in network.generators.iter().enumerate() {
            if !g.in_service {
                continue;
            }
            if let Some(sto) = &g.storage
                && sto.dispatch_mode
                    == surge_network::network::StorageDispatchMode::CostMinimization
            {
                let soc = options
                    .storage_soc_override
                    .as_ref()
                    .and_then(|m| m.get(&gi).copied())
                    .unwrap_or(sto.soc_initial_mwh);
                storage_gen_indices.push(gi);
                storage_soc_mwh.push(soc);
            }
        }
        let storage_bus_idx: Vec<usize> = storage_gen_indices
            .iter()
            .map(|&gi| bus_map[&network.generators[gi].bus])
            .collect();

        let mapping = AcOpfMapping::new(
            &net_context,
            constrained_branches.clone(),
            options.enforce_angle_limits,
            options.optimize_taps,
            options.optimize_phase_shifters,
            options.optimize_switched_shunts,
            options.optimize_svc,
            options.optimize_tcsc,
            hvdc_data.as_ref(),
            storage_bus_idx,
            options.enforce_flowgates,
            options.enforce_capability_curves,
        )?;

        let ybus = build_ybus(network);
        // Pre-compute cumulative Y-bus NNZ offsets for sin/cos cache indexing.
        let n_bus = network.n_buses();
        let mut ybus_row_offsets = vec![0usize; n_bus + 1];
        for i in 0..n_bus {
            ybus_row_offsets[i + 1] = ybus_row_offsets[i] + ybus.row(i).col_idx.len();
        }
        let branch_admittances = build_branch_admittances(network, &constrained_branches, &bus_map);

        // Build flowgate and interface constraint data (pre-computed branch admittances).
        let build_fg_constraint_data =
            |members: &[surge_network::network::WeightedBranchRef]| -> FgConstraintData {
                let mut entries = Vec::with_capacity(members.len());
                for member in members {
                    let coeff = member.coefficient;
                    let br_idx = match net_context.branch_idx_map.get(&(
                        member.branch.from_bus,
                        member.branch.to_bus,
                        member.branch.circuit.clone(),
                    )) {
                        Some(&i) => i,
                        None => continue,
                    };
                    let br = &network.branches[br_idx];
                    if !br.in_service {
                        continue;
                    }
                    let f = bus_map[&br.from_bus];
                    let t = bus_map[&br.to_bus];
                    let mut adm =
                        super::types::compute_branch_admittance(br, f, t, network.base_mva);
                    adm.s_max_sq = 0.0; // flowgates don't use thermal limits
                    entries.push(FgBranchEntry { adm, coeff });
                }
                FgConstraintData {
                    branches: entries,
                    jac_cols: vec![],
                }
            };
        let mut fg_data: Vec<FgConstraintData> = mapping
            .flowgate_indices
            .iter()
            .map(|&fgi| {
                let fg = &network.flowgates[fgi];
                build_fg_constraint_data(&fg.monitored)
            })
            .collect();
        let mut iface_data: Vec<FgConstraintData> = mapping
            .interface_indices
            .iter()
            .map(|&ii| {
                let iface = &network.interfaces[ii];
                build_fg_constraint_data(&iface.members)
            })
            .collect();

        // Build Jacobian sparsity
        let (mut jac_rows, mut jac_cols) = build_jacobian_sparsity(
            &mapping,
            network,
            &ybus,
            &bus_map,
            &branch_admittances,
            hvdc_data.as_ref(),
        );

        // D-curve Jacobian sparsity (empty when enforce_capability_curves=false).
        let pq_constraints = if options.enforce_capability_curves {
            super::pq_curve::build_pq_constraints(
                &mapping.gen_indices,
                &network.generators,
                network.base_mva,
            )
        } else {
            vec![]
        };
        for (ci, c) in pq_constraints.iter().enumerate() {
            let con_row = (mapping.pq_con_offset + ci) as i32;
            jac_rows.push(con_row);
            jac_cols.push(mapping.pg_var(c.gen_local) as i32);
            jac_rows.push(con_row);
            jac_cols.push(mapping.qg_var(c.gen_local) as i32);
        }

        // Flowgate and interface Jacobian sparsity.
        {
            let build_jac_cols_for_fg =
                |fgd: &mut FgConstraintData,
                 row: i32,
                 jac_r: &mut Vec<i32>,
                 jac_c: &mut Vec<i32>| {
                    let mut seen_cols: Vec<usize> = Vec::new();
                    for entry in &fgd.branches {
                        for col in [
                            mapping.va_var(entry.adm.from),
                            mapping.va_var(entry.adm.to),
                            Some(mapping.vm_var(entry.adm.from)),
                            Some(mapping.vm_var(entry.adm.to)),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            if !seen_cols.contains(&col) {
                                jac_r.push(row);
                                jac_c.push(col as i32);
                                seen_cols.push(col);
                            }
                        }
                    }
                    fgd.jac_cols = seen_cols;
                };
            for (fi, fgd) in fg_data.iter_mut().enumerate() {
                let row = (mapping.fg_con_offset + fi) as i32;
                build_jac_cols_for_fg(fgd, row, &mut jac_rows, &mut jac_cols);
            }
            for (ii, ifd) in iface_data.iter_mut().enumerate() {
                let row = (mapping.iface_con_offset + ii) as i32;
                build_jac_cols_for_fg(ifd, row, &mut jac_rows, &mut jac_cols);
            }
        }

        // Benders cut Jacobian sparsity.
        for (c, cut) in context.benders_cuts.iter().enumerate() {
            let con_row = (mapping.n_con + c) as i32;
            for j in 0..cut.alpha.len() {
                jac_rows.push(con_row);
                jac_cols.push((mapping.pg_offset + j) as i32);
            }
        }
        let cuts = context.benders_cuts.clone();

        // Build Hessian sparsity (lower triangle)
        let (mut hess_rows, mut hess_cols, mut hess_map) = build_hessian_sparsity(
            &mapping,
            network,
            &ybus,
            &branch_admittances,
            hvdc_data.as_ref(),
        );

        // Flowgate/interface Hessian sparsity: (Va,Vm) × (Va,Vm) at monitored branch endpoints.
        {
            let mut add_hess = |r: usize, c: usize| {
                let (r, c) = if r >= c { (r, c) } else { (c, r) };
                hess_map.entry((r, c)).or_insert_with(|| {
                    let idx = hess_rows.len();
                    hess_rows.push(r as i32);
                    hess_cols.push(c as i32);
                    idx
                });
            };
            for fgd in fg_data.iter().chain(iface_data.iter()) {
                for entry in &fgd.branches {
                    let va_vars: [Option<usize>; 2] =
                        [mapping.va_var(entry.adm.from), mapping.va_var(entry.adm.to)];
                    let vm_vars: [usize; 2] =
                        [mapping.vm_var(entry.adm.from), mapping.vm_var(entry.adm.to)];
                    for &vai in &va_vars {
                        for &vaj in &va_vars {
                            if let (Some(a), Some(b)) = (vai, vaj) {
                                add_hess(a, b);
                            }
                        }
                    }
                    for &vmi in &vm_vars {
                        for &vmj in &vm_vars {
                            add_hess(vmi, vmj);
                        }
                    }
                    for &vmi in &vm_vars {
                        for &vai in &va_vars {
                            if let Some(a) = vai {
                                add_hess(vmi, a);
                            }
                        }
                    }
                }
            }
        }

        // Build direct-index Hessian lookup for hot-path sections.
        let hess_idx = build_hess_direct_idx(
            &mapping,
            &ybus,
            &ybus_row_offsets,
            &branch_admittances,
            &hess_map,
        );

        // Build initial point (DC warm-start or prior OpfSolution warm-start)
        let x0 = build_initial_point(
            network,
            &mapping,
            context.runtime.warm_start.as_ref(),
            dc_opf_angles,
            hvdc_data.as_ref(),
        );

        Ok(Self {
            network,
            ybus,
            mapping,
            branch_admittances,
            base_mva: network.base_mva,
            bus_map,
            ybus_row_offsets,
            jac_rows,
            jac_cols,
            hess_rows,
            hess_cols,
            hess_map,
            hess_idx,
            x0,
            enforce_thermal_limits: options.enforce_thermal_limits,
            optimize_switched_shunts: options.optimize_switched_shunts,
            optimize_svc: options.optimize_svc,
            optimize_tcsc: options.optimize_tcsc,
            hvdc: hvdc_data,
            storage_gen_indices,
            storage_soc_mwh,
            dt_hours: options.dt_hours.max(1e-6),
            cuts,
            pq_constraints,
            fg_data,
            iface_data,
            bus_pd_mw: network.bus_load_p_mw(),
            bus_qd_mvar: network.bus_load_q_mvar(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ac::types::{AcOpfOptions, AcOpfRunContext};
    use crate::nlp::NlpProblem;
    use surge_network::market::CostCurve;
    use surge_network::network::{
        Branch, Bus, BusType, DcBranch, DcBus, DcConverter, DcConverterStation, Generator, Load,
    };

    fn explicit_hvdc_problem_network(current_max_pu: f64) -> Network {
        let mut net = Network::new("opf_hvdc_bounds");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.buses.push(Bus::new(3, BusType::PQ, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.0));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.0));
        net.loads.push(Load::new(3, 50.0, 20.0));

        let mut slack = Generator::new(1, 80.0, 1.0);
        slack.pmax = 200.0;
        slack.qmax = 200.0;
        slack.qmin = -200.0;
        slack.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, 1.0, 0.0],
        });
        net.generators.push(slack);

        let grid = net.hvdc.ensure_dc_grid(1, None);
        grid.buses.push(DcBus {
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
        grid.buses.push(DcBus {
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
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            id: "conv_a".into(),
            dc_bus: 101,
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
            current_max_pu,
            status: true,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 20.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 80.0,
            active_power_ac_min_mw: 0.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        }));
        grid.converters.push(DcConverter::Vsc(DcConverterStation {
            id: "conv_b".into(),
            dc_bus: 102,
            ac_bus: 3,
            control_type_dc: 2,
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
            current_max_pu,
            status: true,
            loss_constant_mw: 0.0,
            loss_linear: 0.0,
            loss_quadratic_rectifier: 0.0,
            loss_quadratic_inverter: 0.0,
            droop: 0.0,
            power_dc_setpoint_mw: 0.0,
            voltage_dc_setpoint_pu: 1.0,
            active_power_ac_max_mw: 80.0,
            active_power_ac_min_mw: -80.0,
            reactive_power_ac_max_mvar: 50.0,
            reactive_power_ac_min_mvar: -50.0,
        }));
        grid.branches.push(DcBranch {
            id: "branch_a".into(),
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
    fn hvdc_converter_current_bound_uses_raw_current_limit() {
        let net = explicit_hvdc_problem_network(0.6);
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("should build AC-OPF problem");

        let (_, ub) = problem.var_bounds();
        assert!(
            (ub[problem.mapping.iconv_var(0)] - 0.6).abs() < 1e-12,
            "HVDC current bound should use raw current_max_pu"
        );
    }
}
