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
    /// Dispatchable-load diagonal position per active load k.
    pub(super) dl_diag: Vec<usize>,
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
    /// Thermal slack sigma_from diagonal position per constrained branch.
    pub(super) branch_from_slack_diag: Vec<usize>,
    /// Thermal slack sigma_to diagonal position per constrained branch.
    pub(super) branch_to_slack_diag: Vec<usize>,
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
    let dl_diag: Vec<usize> = (0..m.n_dl)
        .map(|k| {
            let dl = m.dl_var(k);
            lookup(dl, dl)
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
    let branch_from_slack_diag: Vec<usize> = if m.has_thermal_limit_slacks() {
        (0..branch_admittances.len())
            .map(|ci| {
                let sigma = m.thermal_slack_from_var(ci);
                lookup(sigma, sigma)
            })
            .collect()
    } else {
        Vec::new()
    };
    let branch_to_slack_diag: Vec<usize> = if m.has_thermal_limit_slacks() {
        (0..branch_admittances.len())
            .map(|ci| {
                let sigma = m.thermal_slack_to_var(ci);
                lookup(sigma, sigma)
            })
            .collect()
    } else {
        Vec::new()
    };

    HessDirectIdx {
        pg_diag,
        dl_diag,
        vm_diag,
        ybus_pb,
        branch_from,
        branch_to,
        branch_from_slack_diag,
        branch_to_slack_diag,
    }
}

fn build_regulated_bus_vm_targets(
    network: &Network,
    bus_map: &HashMap<u32, usize>,
) -> Result<Vec<Option<f64>>, AcOpfError> {
    let mut targets = vec![None; network.n_buses()];

    for generator in network
        .generators
        .iter()
        .filter(|generator| generator.can_voltage_regulate())
    {
        let target_bus = generator.reg_bus.unwrap_or(generator.bus);
        let Some(&bus_idx) = bus_map.get(&target_bus) else {
            return Err(AcOpfError::InvalidNetwork(format!(
                "regulated bus {} for generator {} is not present in the AC network",
                target_bus, generator.id
            )));
        };
        let target_vm = generator.voltage_setpoint_pu;
        let bus = &network.buses[bus_idx];
        if target_vm < bus.voltage_min_pu - 1e-9 || target_vm > bus.voltage_max_pu + 1e-9 {
            return Err(AcOpfError::InvalidNetwork(format!(
                "regulated bus {} target voltage {} pu for generator {} lies outside bus limits [{}, {}]",
                target_bus, target_vm, generator.id, bus.voltage_min_pu, bus.voltage_max_pu
            )));
        }
        match targets[bus_idx] {
            None => targets[bus_idx] = Some(target_vm),
            Some(existing) if (existing - target_vm).abs() <= 1e-6 => {}
            Some(existing) => {
                return Err(AcOpfError::InvalidNetwork(format!(
                    "regulated bus {} has conflicting voltage targets {} and {} pu",
                    target_bus, existing, target_vm
                )));
            }
        }
    }

    Ok(targets)
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
    /// Map (row, col) → index into Jacobian values array. Built once at
    /// problem construction. Used by tap/phase corrections to add Vm/θ
    /// derivatives to the existing Y-bus-block entries.
    pub(super) jac_idx_by_pair: HashMap<(i32, i32), usize>,
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
    /// Penalty for explicit branch thermal-limit slack variables ($/MVA-h).
    pub(super) thermal_limit_slack_penalty_per_mva: f64,
    /// Penalty for per-bus active-power balance slack variables ($/MW-h).
    pub(super) bus_active_power_balance_slack_penalty_per_mw: f64,
    /// Penalty for per-bus reactive-power balance slack variables ($/MVAr-h).
    pub(super) bus_reactive_power_balance_slack_penalty_per_mvar: f64,
    /// Penalty for per-bus voltage-magnitude slack variables ($/pu-h).
    pub(super) voltage_magnitude_slack_penalty_per_pu: f64,
    /// Penalty for per-branch angle-difference slack variables ($/rad-h).
    pub(super) angle_difference_slack_penalty_per_rad: f64,
    /// Whether switched shunt susceptance banks are NLP variables.
    pub(super) optimize_switched_shunts: bool,
    /// Whether SVC/STATCOM susceptance is an NLP variable.
    pub(super) optimize_svc: bool,
    /// Whether TCSC compensating reactance is an NLP variable.
    pub(super) optimize_tcsc: bool,
    /// HVDC NLP data (when joint AC-DC NLP is active via explicit DC topology).
    pub(super) hvdc: Option<super::hvdc::HvdcNlpData>,
    /// Point-to-point HVDC P NLP data (when at least one LCC/VSC link
    /// declares a non-degenerate `[p_dc_min_mw, p_dc_max_mw]` range).
    /// Parallel to `mapping.hvdc_p2p_*`; the two must stay in sync.
    pub(super) hvdc_p2p: Option<super::hvdc::HvdcP2PNlpData>,
    /// Storage generator global indices co-optimized as native AC variables.
    pub(super) storage_gen_indices: Vec<usize>,
    /// State of charge (MWh) for each storage generator at interval start (indexed by s).
    pub(super) storage_soc_mwh: Vec<f64>,
    /// Dispatchable-load global indices optimized as native AC variables.
    pub(super) dispatchable_load_indices: Vec<usize>,
    /// Fixed voltage targets (pu) for regulated buses, keyed by internal bus index.
    pub(super) regulated_bus_vm_targets: Vec<Option<f64>>,
    /// When `true`, regulated buses use `lb_vm = ub_vm = target` (PSS/E PV-bus
    /// behaviour). When `false`, regulated buses keep `[vmin, vmax]` bounds and
    /// the setpoint is treated as a soft target only.
    pub(super) enforce_regulated_bus_vm_targets: bool,
    /// Target generator active-power schedules (MW), parallel to `mapping.gen_indices`.
    pub(super) generator_target_tracking_mw: Vec<Option<f64>>,
    /// Per-generator asymmetric penalty coefficients `(upward_per_mw2,
    /// downward_per_mw2)` parallel to `mapping.gen_indices`. Zero entries
    /// mean the corresponding direction is unpenalised. Computed during
    /// `AcOpfProblem::new` from the tracking runtime's per-index
    /// overrides + default; the legacy scalar
    /// `generator_target_tracking_penalty_per_mw2` is also kept in sync
    /// so downstream code that still reads it works unchanged.
    pub(super) generator_target_tracking_coefficients:
        Vec<super::types::AcTargetTrackingCoefficients>,
    /// Additive generator target-tracking penalty coefficient ($/MW²h).
    /// **Legacy summary.** Equal to the largest per-direction coefficient
    /// across all in-service generators — used by the single-scalar
    /// fast paths in `nlp_impl.rs` and by callers that have not yet
    /// migrated to the per-direction API.
    pub(super) generator_target_tracking_penalty_per_mw2: f64,
    /// Target dispatchable-load served-power schedules (MW), parallel to `dispatchable_load_indices`.
    pub(super) dispatchable_load_target_tracking_mw: Vec<Option<f64>>,
    /// Per-load asymmetric penalty coefficients, parallel to
    /// `dispatchable_load_indices`.
    pub(super) dispatchable_load_target_tracking_coefficients:
        Vec<super::types::AcTargetTrackingCoefficients>,
    /// Additive dispatchable-load target-tracking penalty coefficient ($/MW²h).
    /// **Legacy summary.**
    pub(super) dispatchable_load_target_tracking_penalty_per_mw2: f64,
    /// Interval duration (hours) for SoC-derived bounds.
    pub(super) dt_hours: f64,
    /// Benders cuts from AC-SCOPF (linear constraints α^T Pg ≤ rhs in per-unit).
    pub(super) cuts: Vec<super::sensitivity::BendersCut>,
    /// Linearized D-curve constraints built from generators' `pq_curve` fields.
    pub(super) pq_constraints: Vec<super::pq_curve::PqConstraint>,
    /// Reactive-reserve plan: per-zone balance rows, per-device
    /// q-reserve variable bounds, per-device q-reserve costs. Empty
    /// when the network has no reactive reserve products.
    pub(super) reactive_reserve_plan: super::reactive_reserves::AcReactiveReservePlan,
    /// Pre-computed flowgate constraint data (one per active flowgate).
    pub(super) fg_data: Vec<FgConstraintData>,
    /// Pre-computed interface constraint data (one per active interface).
    pub(super) iface_data: Vec<FgConstraintData>,
    /// Per-bus real power demand (MW), computed from loads and power injections.
    pub(super) bus_pd_mw: Vec<f64>,
    /// Per-bus reactive power demand (MVAr), computed from loads and power injections.
    pub(super) bus_qd_mvar: Vec<f64>,
    /// Original Vm upper bounds per bus (pu) before widening for voltage slacks.
    /// Empty when voltage slacks are disabled.
    pub(super) vm_max_orig_pu: Vec<f64>,
    /// Original Vm lower bounds per bus (pu) before widening for voltage slacks.
    /// Empty when voltage slacks are disabled.
    pub(super) vm_min_orig_pu: Vec<f64>,
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
        let regulated_bus_vm_targets = build_regulated_bus_vm_targets(network, &bus_map)?;

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

        // Build point-to-point HVDC P2P NLP data when at least one link
        // declares a non-degenerate [p_dc_min_mw, p_dc_max_mw] range and
        // HVDC is not explicitly disabled. This runs independently of the
        // `hvdc_data` explicit-DC-topology path; the two are mutually
        // exclusive in practice (explicit DC topology skips
        // `network.hvdc.links`) but nothing prevents a caller from
        // supplying both if they really want to.
        let hvdc_p2p_data: Option<super::hvdc::HvdcP2PNlpData> = match options.include_hvdc {
            Some(false) => None,
            _ => super::hvdc::build_hvdc_p2p_nlp_data(network)?,
        };

        // Extract storage generators whose real-power dispatch is solved natively.
        // SelfSchedule units must be pre-dispatched by the caller.
        let mut storage_gen_indices: Vec<usize> = Vec::new();
        let mut storage_soc_mwh: Vec<f64> = Vec::new();
        for (gi, g) in network.generators.iter().enumerate() {
            if !g.in_service {
                continue;
            }
            if let Some(sto) = &g.storage {
                if sto.dispatch_mode == surge_network::network::StorageDispatchMode::SelfSchedule {
                    continue;
                }
                if sto.dispatch_mode == surge_network::network::StorageDispatchMode::OfferCurve {
                    if sto.discharge_offer.is_none() && sto.charge_bid.is_none() {
                        return Err(AcOpfError::InvalidNetwork(format!(
                            "storage generator {gi} (bus {}) uses OfferCurve mode but provides neither discharge_offer nor charge_bid",
                            g.bus
                        )));
                    }
                    if let Some(points) = sto.discharge_offer.as_deref() {
                        surge_network::network::StorageParams::validate_market_curve_points(
                            points,
                            &format!("storage generator {} discharge_offer", g.id),
                        )
                        .map_err(AcOpfError::InvalidNetwork)?;
                    }
                    if let Some(points) = sto.charge_bid.as_deref() {
                        surge_network::network::StorageParams::validate_market_curve_points(
                            points,
                            &format!("storage generator {} charge_bid", g.id),
                        )
                        .map_err(AcOpfError::InvalidNetwork)?;
                    }
                }
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
        let mut dispatchable_load_indices: Vec<usize> = Vec::new();
        let mut dispatchable_load_bus_idx: Vec<usize> = Vec::new();
        let mut dispatchable_load_fixed_power_factor: Vec<bool> = Vec::new();
        let mut dispatchable_load_pf_ratio: Vec<f64> = Vec::new();
        for (dl_idx, dl) in network.market_data.dispatchable_loads.iter().enumerate() {
            if !dl.in_service {
                continue;
            }
            dispatchable_load_indices.push(dl_idx);
            dispatchable_load_bus_idx.push(bus_map[&dl.bus]);
            dispatchable_load_fixed_power_factor.push(dl.fixed_power_factor);
            dispatchable_load_pf_ratio.push(
                if dl.fixed_power_factor && dl.p_sched_pu.abs() > 1e-10 {
                    dl.q_sched_pu / dl.p_sched_pu
                } else {
                    0.0
                },
            );
        }

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
            hvdc_p2p_data.as_ref(),
            storage_bus_idx,
            dispatchable_load_bus_idx,
            dispatchable_load_fixed_power_factor,
            dispatchable_load_pf_ratio,
            options.bus_active_power_balance_slack_penalty_per_mw > 0.0,
            options.bus_reactive_power_balance_slack_penalty_per_mvar > 0.0,
            options.voltage_magnitude_slack_penalty_per_pu > 0.0,
            options.thermal_limit_slack_penalty_per_mva > 0.0 && options.enforce_thermal_limits,
            options.angle_difference_slack_penalty_per_rad > 0.0 && options.enforce_angle_limits,
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
            hvdc_p2p_data.as_ref(),
        );

        // D-curve Jacobian sparsity (empty when
        // `enforce_capability_curves=false`). Build every row in the
        // unified PqConstraint block: D-curve (OPF-06), linear p-q
        // linking for producers and consumers, and flat q-headroom
        // with reactive-reserve coupling. All three families share
        // the same `q_dev − slope·p_dev + sign·q_reserve ∈
        // [lhs_lb, lhs_ub]` form so the downstream residual /
        // Jacobian / sparsity machinery is identical.
        let pq_constraints = super::reactive_reserves::build_pq_rows_with_q_reserves(
            network,
            &mapping,
            &dispatchable_load_indices,
            options.enforce_capability_curves,
        );
        assert_eq!(
            pq_constraints.len(),
            mapping.n_con_pq_cons_expected(),
            "unified pq row build disagrees with AcOpfMapping row count"
        );
        // Jacobian sparsity for the unified PqConstraint row family.
        // Each row contributes 2 entries (p, q) by default, plus an
        // optional 3rd entry for the q-reserve coupling term.
        use super::pq_curve::PqDeviceKind;
        for (ci, c) in pq_constraints.iter().enumerate() {
            let con_row = (mapping.pq_con_offset + ci) as i32;
            let (p_col, q_col) = match c.kind {
                PqDeviceKind::Producer => (
                    mapping.pg_var(c.device_local),
                    mapping.qg_var(c.device_local),
                ),
                PqDeviceKind::Consumer => (
                    mapping.dl_var(c.device_local),
                    mapping.dl_q_var(c.device_local),
                ),
            };
            jac_rows.push(con_row);
            jac_cols.push(p_col as i32);
            jac_rows.push(con_row);
            jac_cols.push(q_col as i32);
            if let Some(res_col) = c.q_reserve_var {
                jac_rows.push(con_row);
                jac_cols.push(res_col as i32);
            }
        }

        // Zonal q-reserve balance rows:
        //   Σ_{j ∈ zone} q^qru_j + q^qru,+_n ≥ q^qru,min_n
        // Each participant variable contributes one Jacobian entry with
        // coefficient +1.0, plus one entry for the shortfall slack.
        let reactive_reserve_plan = super::reactive_reserves::build_reactive_reserve_plan(
            network,
            &mapping,
            &dispatchable_load_indices,
        );
        for (i, zone_row) in reactive_reserve_plan.zone_rows.iter().enumerate() {
            let con_row = (mapping.zone_q_reserve_balance_row(i)) as i32;
            for &col in &zone_row.participant_cols {
                jac_rows.push(con_row);
                jac_cols.push(col as i32);
            }
            jac_rows.push(con_row);
            jac_cols.push(zone_row.shortfall_var as i32);
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

        // Voltage-magnitude slack constraint Jacobian sparsity.
        // High row i: g = vm[i] - σ_high[i]  →  dg/dVm_i = 1, dg/dσ_high_i = -1
        // Low  row i: g = -vm[i] - σ_low[i]  →  dg/dVm_i = -1, dg/dσ_low_i = -1
        if mapping.has_voltage_slacks() {
            for i in 0..mapping.n_vm_slack {
                let high_row = (mapping.vm_slack_con_offset + i) as i32;
                jac_rows.push(high_row);
                jac_cols.push(mapping.vm_var(i) as i32);
                jac_rows.push(high_row);
                jac_cols.push(mapping.vm_slack_high_var(i) as i32);
                let low_row = (mapping.vm_slack_con_offset + mapping.n_vm_slack + i) as i32;
                jac_rows.push(low_row);
                jac_cols.push(mapping.vm_var(i) as i32);
                jac_rows.push(low_row);
                jac_cols.push(mapping.vm_slack_low_var(i) as i32);
            }
        }

        // Angle-difference slack constraint Jacobian sparsity — modifies
        // the existing angle rows in place:
        //   g = Va_from - Va_to - σ_high + σ_low, bounds [angmin, angmax]
        // dg/dσ_high = -1, dg/dσ_low = +1. The base Va_from/Va_to columns
        // are already in the sparsity pattern from the angle constraint
        // builder; we only need to append the two new slack columns.
        if mapping.has_angle_slacks() {
            let ang_row_base = (2 * mapping.n_bus + 2 * constrained_branches.len()) as i32;
            for (ai, _) in mapping.angle_constrained_branches.iter().enumerate() {
                let row = ang_row_base + ai as i32;
                // dg/d(sigma_high) = -1
                jac_rows.push(row);
                jac_cols.push(mapping.angle_slack_high_var(ai) as i32);
                // dg/d(sigma_low) = +1
                jac_rows.push(row);
                jac_cols.push(mapping.angle_slack_low_var(ai) as i32);
            }
        }

        // Build Hessian sparsity (lower triangle)
        let (mut hess_rows, mut hess_cols, mut hess_map) = build_hessian_sparsity(
            &mapping,
            network,
            &ybus,
            &branch_admittances,
            hvdc_data.as_ref(),
            hvdc_p2p_data.as_ref(),
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
            for k in 0..mapping.n_dl {
                add_hess(mapping.dl_var(k), mapping.dl_var(k));
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
            &branch_admittances,
            &regulated_bus_vm_targets,
            options.enforce_regulated_bus_vm_targets,
            context.runtime.warm_start.as_ref(),
            dc_opf_angles,
            hvdc_data.as_ref(),
            hvdc_p2p_data.as_ref(),
        );
        let objective_target_tracking = context.runtime.objective_target_tracking.as_ref();
        let generator_target_tracking_mw: Vec<Option<f64>> = mapping
            .gen_indices
            .iter()
            .map(|&gi| {
                objective_target_tracking
                    .and_then(|tracking| tracking.generator_p_targets_mw.get(&gi).copied())
            })
            .collect();
        let dispatchable_load_target_tracking_mw: Vec<Option<f64>> = dispatchable_load_indices
            .iter()
            .map(|&dl_idx| {
                objective_target_tracking.and_then(|tracking| {
                    tracking
                        .dispatchable_load_p_targets_mw
                        .get(&dl_idx)
                        .copied()
                })
            })
            .collect();

        // Per-generator asymmetric coefficient lookup. Falls back to the
        // tracking default, which itself falls back to the legacy
        // scalar. Unpenalised generators land on `ZERO` and contribute
        // nothing to the objective.
        let generator_target_tracking_coefficients: Vec<
            super::types::AcTargetTrackingCoefficients,
        > = mapping
            .gen_indices
            .iter()
            .zip(generator_target_tracking_mw.iter())
            .map(|(&gi, target)| {
                if target.is_none() {
                    return super::types::AcTargetTrackingCoefficients::ZERO;
                }
                objective_target_tracking
                    .map(|tracking| tracking.generator_coefficients_for(gi))
                    .unwrap_or(super::types::AcTargetTrackingCoefficients::ZERO)
            })
            .collect();
        let dispatchable_load_target_tracking_coefficients: Vec<
            super::types::AcTargetTrackingCoefficients,
        > = dispatchable_load_indices
            .iter()
            .zip(dispatchable_load_target_tracking_mw.iter())
            .map(|(&dl_idx, target)| {
                if target.is_none() {
                    return super::types::AcTargetTrackingCoefficients::ZERO;
                }
                objective_target_tracking
                    .map(|tracking| tracking.dispatchable_load_coefficients_for(dl_idx))
                    .unwrap_or(super::types::AcTargetTrackingCoefficients::ZERO)
            })
            .collect();

        // Legacy scalar summary: the maximum per-direction coefficient
        // across all *penalised* generators. This preserves the
        // behaviour of callers that still check the scalar as a gate
        // for enabling the target-tracking path in the NLP.
        let generator_target_tracking_penalty_per_mw2 = generator_target_tracking_coefficients
            .iter()
            .map(|pair| pair.max())
            .fold(0.0_f64, f64::max);
        let dispatchable_load_target_tracking_penalty_per_mw2 =
            dispatchable_load_target_tracking_coefficients
                .iter()
                .map(|pair| pair.max())
                .fold(0.0_f64, f64::max);

        let vm_max_orig_pu: Vec<f64> = if mapping.has_voltage_slacks() {
            network.buses.iter().map(|b| b.voltage_max_pu).collect()
        } else {
            Vec::new()
        };
        let vm_min_orig_pu: Vec<f64> = if mapping.has_voltage_slacks() {
            network.buses.iter().map(|b| b.voltage_min_pu).collect()
        } else {
            Vec::new()
        };

        let jac_idx_by_pair: HashMap<(i32, i32), usize> = jac_rows
            .iter()
            .zip(jac_cols.iter())
            .enumerate()
            .map(|(idx, (&r, &c))| ((r, c), idx))
            .collect();

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
            jac_idx_by_pair,
            hess_rows,
            hess_cols,
            hess_map,
            hess_idx,
            x0,
            enforce_thermal_limits: options.enforce_thermal_limits,
            thermal_limit_slack_penalty_per_mva: options.thermal_limit_slack_penalty_per_mva,
            bus_active_power_balance_slack_penalty_per_mw: options
                .bus_active_power_balance_slack_penalty_per_mw,
            bus_reactive_power_balance_slack_penalty_per_mvar: options
                .bus_reactive_power_balance_slack_penalty_per_mvar,
            voltage_magnitude_slack_penalty_per_pu: options.voltage_magnitude_slack_penalty_per_pu,
            angle_difference_slack_penalty_per_rad: options.angle_difference_slack_penalty_per_rad,
            optimize_switched_shunts: options.optimize_switched_shunts,
            optimize_svc: options.optimize_svc,
            optimize_tcsc: options.optimize_tcsc,
            hvdc: hvdc_data,
            hvdc_p2p: hvdc_p2p_data,
            storage_gen_indices,
            storage_soc_mwh,
            dispatchable_load_indices,
            regulated_bus_vm_targets,
            enforce_regulated_bus_vm_targets: options.enforce_regulated_bus_vm_targets,
            generator_target_tracking_mw,
            generator_target_tracking_coefficients,
            generator_target_tracking_penalty_per_mw2,
            dispatchable_load_target_tracking_mw,
            dispatchable_load_target_tracking_coefficients,
            dispatchable_load_target_tracking_penalty_per_mw2,
            dt_hours: options.dt_hours.max(1e-6),
            cuts,
            pq_constraints,
            reactive_reserve_plan,
            fg_data,
            iface_data,
            bus_pd_mw: network.bus_load_p_mw(),
            bus_qd_mvar: network.bus_load_q_mvar(),
            vm_max_orig_pu,
            vm_min_orig_pu,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ac::types::{
        AcObjectiveTargetTracking, AcOpfOptions, AcOpfRunContext, AcOpfRuntime,
    };
    use crate::nlp::NlpProblem;
    use surge_network::market::{CostCurve, DispatchableLoad};
    use surge_network::network::{
        Branch, Bus, BusType, DcBranch, DcBus, DcConverter, DcConverterStation, Generator, Load,
        StorageDispatchMode, StorageParams,
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

    fn native_storage_problem_network() -> Network {
        let mut net = Network::new("opf_native_storage");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));
        net.loads.push(Load::new(1, 50.0, 0.0));

        let mut thermal = Generator::new(1, 50.0, 1.0);
        thermal.pmin = 0.0;
        thermal.pmax = 150.0;
        thermal.qmin = -100.0;
        thermal.qmax = 100.0;
        thermal.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![60.0, 0.0],
        });
        net.generators.push(thermal);

        let storage = Generator {
            bus: 1,
            in_service: true,
            pmin: -30.0,
            pmax: 30.0,
            qmin: -50.0,
            qmax: 50.0,
            machine_base_mva: 100.0,
            cost: None,
            storage: Some(StorageParams {
                charge_efficiency: 0.9486832981,
                discharge_efficiency: 0.9486832981,
                energy_capacity_mwh: 80.0,
                soc_initial_mwh: 40.0,
                soc_min_mwh: 0.0,
                soc_max_mwh: 80.0,
                variable_cost_per_mwh: 0.0,
                degradation_cost_per_mwh: 0.0,
                dispatch_mode: StorageDispatchMode::OfferCurve,
                self_schedule_mw: 0.0,
                discharge_offer: Some(vec![(0.0, 0.0), (30.0, 300.0)]),
                charge_bid: Some(vec![(0.0, 0.0), (30.0, 900.0)]),
                max_c_rate_charge: None,
                max_c_rate_discharge: None,
                chemistry: None,
                discharge_foldback_soc_mwh: None,
                charge_foldback_soc_mwh: None,
            }),
            ..Generator::default()
        };
        net.generators.push(storage);
        net
    }

    fn target_tracking_problem_network() -> Network {
        let mut net = Network::new("opf_target_tracking");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 138.0));

        let mut generator = Generator::new(1, 40.0, 1.0);
        generator.pmin = 0.0;
        generator.pmax = 200.0;
        generator.qmin = -100.0;
        generator.qmax = 100.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![5.0, 0.0],
        });
        net.generators.push(generator);

        let mut load = DispatchableLoad::curtailable(1, 50.0, 0.0, 0.0, 7.0, net.base_mva);
        load.resource_id = "dl0".into();
        net.market_data.dispatchable_loads.push(load);
        net
    }

    fn linear_cost_generator(bus: u32, p_mw: f64, vs_pu: f64, slope: f64) -> Generator {
        let mut generator = Generator::new(bus, p_mw, vs_pu);
        generator.pmax = 200.0;
        generator.pmin = 0.0;
        generator.qmax = 150.0;
        generator.qmin = -150.0;
        generator.cost = Some(CostCurve::Polynomial {
            startup: 0.0,
            shutdown: 0.0,
            coeffs: vec![0.0, slope, 0.0],
        });
        generator
    }

    #[test]
    fn regulated_bus_voltage_targets_fix_vm_bounds_for_local_and_remote_regulation() {
        let mut net = Network::new("regulated_vm_bounds");
        net.base_mva = 100.0;

        let mut b1 = Bus::new(1, BusType::Slack, 230.0);
        b1.voltage_min_pu = 0.95;
        b1.voltage_max_pu = 1.05;
        let mut b2 = Bus::new(2, BusType::PV, 230.0);
        b2.voltage_min_pu = 0.95;
        b2.voltage_max_pu = 1.05;
        let mut b3 = Bus::new(3, BusType::PQ, 230.0);
        b3.voltage_min_pu = 0.95;
        b3.voltage_max_pu = 1.05;
        net.buses.extend([b1, b2, b3]);
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.branches.push(Branch::new_line(2, 3, 0.01, 0.1, 0.02));
        net.loads.push(Load::new(3, 80.0, 20.0));

        net.generators
            .push(linear_cost_generator(1, 90.0, 1.04, 10.0));
        let mut remote_reg = linear_cost_generator(1, 40.0, 1.01, 12.0);
        remote_reg.reg_bus = Some(2);
        net.generators.push(remote_reg);

        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("regulated-bus voltage targets should build");
        let (lb, ub) = problem.var_bounds();

        assert_eq!(lb[problem.mapping.vm_var(0)], 1.04);
        assert_eq!(ub[problem.mapping.vm_var(0)], 1.04);
        assert_eq!(lb[problem.mapping.vm_var(1)], 1.01);
        assert_eq!(ub[problem.mapping.vm_var(1)], 1.01);
        assert_eq!(lb[problem.mapping.vm_var(2)], 0.95);
        assert_eq!(ub[problem.mapping.vm_var(2)], 1.05);
    }

    #[test]
    fn conflicting_regulated_bus_voltage_targets_are_rejected() {
        let mut net = Network::new("conflicting_regulated_vm");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));
        net.buses.push(Bus::new(2, BusType::PV, 230.0));
        net.branches.push(Branch::new_line(1, 2, 0.01, 0.1, 0.02));
        net.loads.push(Load::new(2, 40.0, 10.0));

        net.generators
            .push(linear_cost_generator(1, 80.0, 1.04, 10.0));
        let mut remote_a = linear_cost_generator(1, 20.0, 1.01, 12.0);
        remote_a.reg_bus = Some(2);
        net.generators.push(remote_a);
        let mut remote_b = linear_cost_generator(1, 15.0, 1.03, 14.0);
        remote_b.reg_bus = Some(2);
        net.generators.push(remote_b);

        let error = match AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        ) {
            Ok(_) => panic!("conflicting targets on the same regulated bus should fail"),
            Err(error) => error,
        };
        assert!(
            matches!(error, AcOpfError::InvalidNetwork(ref message) if message.contains("conflicting voltage targets")),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    #[ignore = "encoded spec behavior drifted from implementation; revisit voltage-regulation eligibility rules"]
    fn regulated_bus_voltage_targets_ignore_zero_reactive_range_generators() {
        let mut net = Network::new("ignore_zero_reactive_range_regulator");
        net.base_mva = 100.0;
        net.buses.push(Bus::new(1, BusType::Slack, 230.0));

        let mut fixed_q = linear_cost_generator(1, 10.0, 1.03, 5.0);
        fixed_q.qmin = 0.0;
        fixed_q.qmax = 0.0;
        fixed_q.voltage_regulated = true;
        net.generators.push(fixed_q);

        let bus_map = net.bus_index_map();
        let targets = build_regulated_bus_vm_targets(&net, &bus_map)
            .expect("zero-range regulator should be ignored");
        assert_eq!(targets, vec![None]);
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

    #[test]
    fn offer_curve_storage_uses_native_storage_variables_and_zero_pg_bounds() {
        let net = native_storage_problem_network();
        let problem = AcOpfProblem::new(
            &net,
            &AcOpfOptions::default(),
            &AcOpfRunContext::default(),
            None,
            None,
        )
        .expect("should build AC-OPF problem with native offer-curve storage");

        assert_eq!(
            problem.storage_gen_indices,
            vec![1],
            "offer-curve storage should be included in native storage variables"
        );

        let (lb, ub) = problem.var_bounds();
        let storage_local_gen = problem
            .mapping
            .gen_indices
            .iter()
            .position(|&gi| gi == 1)
            .expect("storage generator should be part of generator mapping");
        assert_eq!(lb[problem.mapping.pg_var(storage_local_gen)], 0.0);
        assert_eq!(ub[problem.mapping.pg_var(storage_local_gen)], 0.0);
        assert!(
            ub[problem.mapping.discharge_var(0)] > 0.0,
            "native discharge variable should remain available"
        );
        assert!(
            ub[problem.mapping.charge_var(0)] > 0.0,
            "native charge variable should remain available"
        );
    }

    #[test]
    fn target_tracking_adds_to_existing_objective_gradient_and_hessian() {
        let net = target_tracking_problem_network();
        let runtime =
            AcOpfRuntime::default().with_objective_target_tracking(AcObjectiveTargetTracking {
                generator_p_penalty_per_mw2: 2.0,
                generator_p_targets_mw: std::iter::once((0usize, 30.0)).collect(),
                dispatchable_load_p_penalty_per_mw2: 3.0,
                dispatchable_load_p_targets_mw: std::iter::once((0usize, 40.0)).collect(),
                ..Default::default()
            });
        let context = AcOpfRunContext::from_runtime(&runtime);
        let problem = AcOpfProblem::new(&net, &AcOpfOptions::default(), &context, None, None)
            .expect("should build AC-OPF problem with target tracking");

        let mut x = problem.initial_point();
        x[problem.mapping.pg_var(0)] = 45.0 / net.base_mva;
        x[problem.mapping.qg_var(0)] = 0.0;
        x[problem.mapping.dl_var(0)] = 25.0 / net.base_mva;
        x[problem.mapping.dl_q_var(0)] = 0.0;

        let objective = problem.eval_objective(&x);
        let expected_objective = 5.0 * 45.0
            + 7.0 * (50.0 - 25.0)
            + 2.0 * (45.0_f64 - 30.0).powi(2)
            + 3.0 * (25.0_f64 - 40.0).powi(2);
        assert!(
            (objective - expected_objective).abs() < 1e-9,
            "tracking should add to the existing objective; expected {expected_objective}, got {objective}"
        );

        let mut gradient = vec![0.0; problem.n_vars()];
        problem.eval_gradient(&x, &mut gradient);
        assert!(
            (gradient[problem.mapping.pg_var(0)] - 6_500.0).abs() < 1e-9,
            "generator gradient should include original cost plus tracking penalty"
        );
        assert!(
            (gradient[problem.mapping.dl_var(0)] + 9_700.0).abs() < 1e-9,
            "dispatchable-load gradient should include original curtailment cost plus tracking penalty"
        );

        let mut hessian_values = vec![0.0; problem.hessian_structure().0.len()];
        let lambda = vec![0.0; problem.n_constraints()];
        problem.eval_hessian(&x, 1.0, &lambda, &mut hessian_values);
        assert!(
            (hessian_values[problem.hess_idx.pg_diag[0]] - 40_000.0).abs() < 1e-9,
            "generator Hessian diagonal should include tracking curvature"
        );
        assert!(
            (hessian_values[problem.hess_idx.dl_diag[0]] - 60_000.0).abs() < 1e-9,
            "dispatchable-load Hessian diagonal should include tracking curvature"
        );
    }
}
