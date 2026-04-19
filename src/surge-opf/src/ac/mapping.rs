// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! AC-OPF variable index mapping.
//!
//! Maps NLP variables to their physical meaning: voltage angles, magnitudes,
//! generator dispatch, transformer taps, phase shifters, shunts, FACTS, HVDC,
//! storage, dispatchable loads, and optional thermal-limit slacks.

use surge_network::Network;
use surge_network::market::ReserveKind;

use crate::common::context::OpfNetworkContext;

use super::types::{AcOpfError, SvcOpfData, TcscOpfData};

/// Maps variables in the NLP vector to their physical meaning.
///
/// Variable layout:
/// `[Va(n-1) | Vm(n) | Pg(ng) | Qg(ng) | qru_pr(n_qr_pr) | qrd_pr(n_qr_pr) | τ(n_tap) | θ_s(n_ps) | b_sw(n_sw) | b_svc(n_svc) | x_comp(n_tcsc) | P_conv(n_conv) | Q_conv(n_conv) | V_dc(n_dc_bus) | I_conv(n_conv) | dis(n_sto) | ch(n_sto) | p_served(n_dl) | q_served(n_dl) | qru_cs(n_qr_cs) | qrd_cs(n_qr_cs) | p_slack_pos(n_p_bus) | p_slack_neg(n_p_bus) | q_slack_pos(n_q_bus) | q_slack_neg(n_q_bus) | vm_slack_high(n_vm_slack) | vm_slack_low(n_vm_slack) | sigma_from(n_br) | sigma_to(n_br) | zone_qru_shortfall(n_zqru) | zone_qrd_shortfall(n_zqrd) | angle_slack_high(n_ang) | angle_slack_low(n_ang)]`
///
/// The `qru_pr`/`qrd_pr` and `qru_cs`/`qrd_cs` blocks implement GO
/// Competition Challenge 3 §4.6 reactive reserves (eqs 88-89, 112-128).
/// They are allocated only when the network has at least one
/// `ReserveProduct` with `kind = Reactive`; otherwise their sizes and
/// offsets collapse so the layout is identical to the pre-5B mapping.
pub(crate) struct AcOpfMapping {
    pub(crate) n_bus: usize,
    pub(crate) slack_idx: usize,
    /// In-service generator indices into network.generators[].
    pub(crate) gen_indices: Vec<usize>,
    pub(crate) n_gen: usize,
    /// Map from bus internal index to list of local gen indices at that bus.
    pub(crate) bus_gen_map: Vec<Vec<usize>>,
    /// Constrained branch indices (thermal flow limits).
    pub(crate) constrained_branches: Vec<usize>,
    /// Angle-difference constrained branch indices and their (angmin, angmax) in radians.
    ///
    /// Only includes branches where at least one limit is tighter than ±π.
    pub(crate) angle_constrained_branches: Vec<(usize, f64, f64)>,
    /// Tap-control branch indices with (branch_idx, tap_min, tap_max).
    pub(crate) tap_ctrl_branches: Vec<(usize, f64, f64)>,
    /// Phase-shift-control branch indices with (branch_idx, phase_min_rad, phase_max_rad).
    pub(crate) ps_ctrl_branches: Vec<(usize, f64, f64)>,
    /// Total number of variables.
    pub(crate) n_var: usize,
    /// Total number of constraints (including D-curve, excluding Benders cuts).
    pub(crate) n_con: usize,
    /// Constraint row offset where D-curve (P-Q capability) constraints start.
    pub(crate) pq_con_offset: usize,
    // Variable offsets
    pub(crate) va_offset: usize, // 0
    pub(crate) vm_offset: usize, // n_bus - 1
    pub(crate) pg_offset: usize, // n_bus - 1 + n_bus
    pub(crate) qg_offset: usize, // n_bus - 1 + n_bus + n_gen
    /// Offset of `q^qru_pr[0]` — reactive-up reserve (producers).
    /// 0-sized block when the network has no reactive reserve
    /// products.
    pub(crate) producer_q_reserve_up_offset: usize,
    /// Offset of `q^qrd_pr[0]` — reactive-down reserve (producers).
    pub(crate) producer_q_reserve_down_offset: usize,
    /// Number of allocated producer q-reserve columns per direction.
    /// Equals `n_gen` when reactive reserves are active, `0` otherwise.
    pub(crate) n_producer_q_reserve: usize,
    /// Offset of τ[0] (after Qg, after the optional producer q-reserve blocks).
    pub(crate) tap_offset: usize,
    /// Offset of θ_s[0] (after τ).
    pub(crate) ps_offset: usize,
    /// Offset of b_sw[0] (after θ_s).
    pub(crate) sw_offset: usize,
    /// Number of switched shunt OPF variables.
    pub(crate) n_sw: usize,
    /// Internal bus index for each switched-shunt OPF variable.
    pub(crate) switched_shunt_bus_idx: Vec<usize>,
    /// Offset of b_svc[0] (after b_sw).
    pub(crate) svc_offset: usize,
    /// Number of SVC NLP variables.
    pub(crate) n_svc: usize,
    /// SVC device data.
    pub(crate) svc_devices: Vec<SvcOpfData>,
    /// Offset of x_comp[0] (after b_svc).
    pub(crate) tcsc_offset: usize,
    /// Number of TCSC NLP variables.
    pub(crate) n_tcsc: usize,
    /// TCSC device data.
    pub(crate) tcsc_devices: Vec<TcscOpfData>,
    // --- HVDC NLP augmentation ---
    /// Offset of P_conv[0] (after x_comp).
    pub(crate) pconv_offset: usize,
    /// Offset of Q_conv[0] (after P_conv).
    pub(crate) qconv_offset: usize,
    /// Offset of V_dc[0] (after Q_conv).
    pub(crate) vdc_offset: usize,
    /// Offset of I_conv[0] (after V_dc).
    pub(crate) iconv_offset: usize,
    /// Number of DC converters (HVDC NLP variables).
    pub(crate) n_conv: usize,
    /// Number of DC buses (HVDC NLP variables).
    pub(crate) n_dc_bus: usize,
    /// AC bus internal index for each converter k.
    pub(crate) conv_ac_bus: Vec<usize>,
    /// Constraint row offset where HVDC DC KCL constraints begin.
    pub(crate) dc_kcl_row_offset: usize,
    /// Constraint row offset where HVDC current-definition constraints begin.
    pub(crate) iconv_eq_row_offset: usize,
    /// Constraint row offset where HVDC DC-control constraints begin.
    pub(crate) dc_control_row_offset: usize,
    /// Constraint row offset where HVDC AC-control constraints begin.
    pub(crate) ac_control_row_offset: usize,
    // --- Storage NLP variables ---
    /// Number of storage units co-optimized as native AC variables.
    pub(crate) n_sto: usize,
    /// Offset of dis[0] in variable vector (after I_conv).
    pub(crate) discharge_offset: usize,
    /// Offset of ch[0] in variable vector (after dis).
    pub(crate) charge_offset: usize,
    /// Internal bus index for each storage unit s.
    pub(crate) storage_bus_idx: Vec<usize>,
    /// Number of dispatchable loads optimized as native AC variables.
    pub(crate) n_dl: usize,
    /// Offset of p_served[0] in variable vector (after storage).
    pub(crate) dl_offset: usize,
    /// Offset of q_served[0] in variable vector (after dispatchable-load P).
    pub(crate) dl_q_offset: usize,
    /// Offset of `q^qru_cs[0]` — reactive-up reserve (consumers).
    /// 0-sized block when the network has no reactive reserve
    /// products.
    pub(crate) consumer_q_reserve_up_offset: usize,
    /// Offset of `q^qrd_cs[0]` — reactive-down reserve (consumers).
    pub(crate) consumer_q_reserve_down_offset: usize,
    /// Number of allocated consumer q-reserve columns per direction.
    /// Equals `n_dl` when reactive reserves are active, `0` otherwise.
    pub(crate) n_consumer_q_reserve: usize,
    /// Internal bus index for each dispatchable load k.
    pub(crate) dispatchable_load_bus_idx: Vec<usize>,
    /// Fixed-power-factor flag for each dispatchable load k.
    pub(crate) dispatchable_load_fixed_power_factor: Vec<bool>,
    /// Fixed power-factor ratio Q/P for each dispatchable load k.
    pub(crate) dispatchable_load_pf_ratio: Vec<f64>,
    /// Equality-constraint row index for each fixed-power-factor load.
    pub(crate) dispatchable_load_pf_rows: Vec<Option<usize>>,
    /// Number of constrained branches with explicit thermal-limit slack variables.
    pub(crate) n_branch_thermal_slack: usize,
    /// Number of buses with explicit active-power balance slack variable pairs.
    pub(crate) n_p_bus_balance_slack: usize,
    /// Number of buses with explicit reactive-power balance slack variable pairs.
    pub(crate) n_q_bus_balance_slack: usize,
    /// Offset of positive active-balance slack variables in the variable vector.
    pub(crate) p_balance_slack_pos_offset: usize,
    /// Offset of negative active-balance slack variables in the variable vector.
    pub(crate) p_balance_slack_neg_offset: usize,
    /// Offset of positive reactive-balance slack variables in the variable vector.
    pub(crate) q_balance_slack_pos_offset: usize,
    /// Offset of negative reactive-balance slack variables in the variable vector.
    pub(crate) q_balance_slack_neg_offset: usize,
    /// Number of voltage-magnitude slack variables per direction (n_bus when enabled, 0 otherwise).
    pub(crate) n_vm_slack: usize,
    /// Offset of σ_high[0] (voltage-magnitude high slack) in the variable vector.
    pub(crate) vm_slack_high_offset: usize,
    /// Offset of σ_low[0] (voltage-magnitude low slack) in the variable vector.
    pub(crate) vm_slack_low_offset: usize,
    /// Offset of sigma_from[0] in the variable vector (after dispatchable loads).
    pub(crate) thermal_slack_from_offset: usize,
    /// Offset of sigma_to[0] in the variable vector (after from-side thermal slacks).
    pub(crate) thermal_slack_to_offset: usize,
    /// Offset of the first zonal reactive-up shortfall slack column.
    /// One entry per active `(zone, up-product)` pair. Size is
    /// `n_zone_q_reserve_up_shortfall`.
    pub(crate) zone_q_reserve_up_shortfall_offset: usize,
    /// Offset of the first zonal reactive-down shortfall slack column.
    pub(crate) zone_q_reserve_down_shortfall_offset: usize,
    /// Number of zonal reactive-up shortfall columns (= number of
    /// distinct `(zone, up-product)` pairs with non-trivial
    /// requirement).
    pub(crate) n_zone_q_reserve_up_shortfall: usize,
    /// Number of zonal reactive-down shortfall columns.
    pub(crate) n_zone_q_reserve_down_shortfall: usize,
    /// Row offset where zonal q-reserve balance rows begin (after
    /// `pq_con` and before dispatchable-load PF equality rows). The
    /// block contains `n_zone_q_reserve_up_shortfall +
    /// n_zone_q_reserve_down_shortfall` rows: the up rows come first,
    /// then the down rows.
    pub(crate) zone_q_reserve_balance_row_offset: usize,
    /// Active flowgate indices into `network.flowgates` (in_service only).
    pub(crate) flowgate_indices: Vec<usize>,
    /// Active interface indices into `network.interfaces` (in_service, limit > 0).
    pub(crate) interface_indices: Vec<usize>,
    /// Constraint row offset where flowgate constraints begin.
    pub(crate) fg_con_offset: usize,
    /// Constraint row offset where interface constraints begin.
    pub(crate) iface_con_offset: usize,
    /// Constraint row offset where voltage-magnitude slack constraints begin.
    /// Contains 2*n_vm_slack rows: first n_vm_slack rows for high-side
    /// (`vm[i] - σ_high[i] ≤ vm_max[i]`), then n_vm_slack rows for low-side
    /// (`-vm[i] - σ_low[i] ≤ -vm_min[i]`).
    pub(crate) vm_slack_con_offset: usize,
    /// Number of angle-difference slack variable pairs.
    /// Equals `angle_constrained_branches.len()` when enabled, `0` otherwise.
    pub(crate) n_angle_slack: usize,
    /// Offset of sigma_high[0] (angle-difference high slack) in the variable vector.
    pub(crate) angle_slack_high_offset: usize,
    /// Offset of sigma_low[0] (angle-difference low slack) in the variable vector.
    pub(crate) angle_slack_low_offset: usize,
    // --- HVDC point-to-point P variables (joint AC-DC OPF) ---
    /// Number of point-to-point HVDC P decision variables.
    ///
    /// Each in-service link with `p_dc_min_mw < p_dc_max_mw` contributes
    /// one variable here. The block sits at the very end of the variable
    /// vector so adding it does not shift any existing offsets.
    pub(crate) n_hvdc_p2p_links: usize,
    /// Offset of the first HVDC P2P decision variable (after
    /// `angle_slack_low`). Size `n_hvdc_p2p_links`.
    pub(crate) hvdc_p2p_offset: usize,
    /// From-bus internal index for each P2P HVDC link. Used by the bus
    /// P balance contribution in `eval_constraints` / `eval_jacobian`.
    pub(crate) hvdc_p2p_from_bus_idx: Vec<usize>,
    /// To-bus internal index for each P2P HVDC link.
    pub(crate) hvdc_p2p_to_bus_idx: Vec<usize>,
    /// Quadratic loss coefficient (pu) for each P2P HVDC link. Zero for
    /// lossless links. Split 50/50 between the two terminals in the bus
    /// P balance: `g[from] += Pg + 0.5*c*Pg²`, `g[to] -= Pg - 0.5*c*Pg²`.
    pub(crate) hvdc_p2p_loss_c_pu: Vec<f64>,
}

impl AcOpfMapping {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        context: &OpfNetworkContext<'_>,
        constrained_branches: Vec<usize>,
        enforce_angle_limits: bool,
        optimize_taps: bool,
        optimize_phase_shifters: bool,
        optimize_switched_shunts: bool,
        optimize_svc: bool,
        optimize_tcsc: bool,
        hvdc: Option<&super::hvdc::HvdcNlpData>,
        hvdc_p2p: Option<&super::hvdc::HvdcP2PNlpData>,
        storage_bus_idx: Vec<usize>,
        dispatchable_load_bus_idx: Vec<usize>,
        dispatchable_load_fixed_power_factor: Vec<bool>,
        dispatchable_load_pf_ratio: Vec<f64>,
        enable_p_bus_balance_slacks: bool,
        enable_q_bus_balance_slacks: bool,
        enable_voltage_slacks: bool,
        enable_thermal_limit_slacks: bool,
        enable_angle_slacks: bool,
        enforce_flowgates: bool,
        enforce_capability_curves: bool,
    ) -> Result<Self, AcOpfError> {
        use surge_network::network::{PhaseMode, TapMode};

        let network: &Network = context.network;
        let n_bus = network.n_buses();
        let slack_idx = context.slack_idx;
        let gen_indices = context.gen_indices.clone();
        let n_gen = gen_indices.len();

        let bus_map = &context.bus_map;
        let bus_gen_map = context.bus_gen_map.clone();

        let n_va = n_bus - 1; // skip slack
        let n_vm = n_bus;

        // Collect tap-controllable branches.
        let mut tap_ctrl_branches: Vec<(usize, f64, f64)> = Vec::new();
        if optimize_taps {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if let Some(ctrl) = &br.opf_control {
                    if br.in_service && ctrl.tap_mode == TapMode::Continuous {
                        tap_ctrl_branches.push((br_idx, ctrl.tap_min, ctrl.tap_max));
                    }
                }
            }
        }
        let n_tap = tap_ctrl_branches.len();

        // Collect phase-shift-controllable branches.
        let mut ps_ctrl_branches: Vec<(usize, f64, f64)> = Vec::new();
        if optimize_phase_shifters {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if let Some(ctrl) = &br.opf_control {
                    if br.in_service && ctrl.phase_mode == PhaseMode::Continuous {
                        let lo_rad = ctrl.phase_min_rad;
                        let hi_rad = ctrl.phase_max_rad;
                        ps_ctrl_branches.push((br_idx, lo_rad, hi_rad));
                    }
                }
            }
        }
        let n_ps = ps_ctrl_branches.len();

        let switched_shunt_bus_idx: Vec<usize> = if optimize_switched_shunts {
            let mut resolved = Vec::with_capacity(network.controls.switched_shunts_opf.len());
            for shunt in &network.controls.switched_shunts_opf {
                let bus_idx = *bus_map.get(&shunt.bus).ok_or_else(|| {
                    AcOpfError::InvalidNetwork(format!(
                        "switched shunt OPF `{}` references unknown host bus {}",
                        shunt.id, shunt.bus
                    ))
                })?;
                resolved.push(bus_idx);
            }
            resolved
        } else {
            vec![]
        };
        let n_sw = switched_shunt_bus_idx.len();

        // Collect SVC/STATCOM devices for optimization.
        let base_mva = network.base_mva;
        let svc_devices: Vec<SvcOpfData> = if optimize_svc {
            network
                .facts_devices
                .iter()
                .filter(|f| f.mode.in_service() && f.mode.has_shunt())
                .filter_map(|f| {
                    let bus_idx = *bus_map.get(&f.bus_from)?;
                    let b_max = f.q_max / base_mva; // capacitive limit
                    let b_min = if f.q_min != 0.0 {
                        f.q_min / base_mva
                    } else {
                        -f.q_max / base_mva
                    };
                    let b_init = (f.q_setpoint_mvar / base_mva).clamp(b_min, b_max);
                    Some(SvcOpfData {
                        bus_idx,
                        b_min,
                        b_max,
                        b_init,
                    })
                })
                .collect()
        } else {
            vec![]
        };
        let n_svc = svc_devices.len();

        // Collect TCSC devices for optimization.
        let tcsc_devices: Vec<TcscOpfData> = if optimize_tcsc {
            network
                .facts_devices
                .iter()
                .filter(|f| f.mode.in_service() && f.mode.has_series() && f.bus_to != 0)
                .filter_map(|f| {
                    let from_idx = *bus_map.get(&f.bus_from)?;
                    let to_idx = *bus_map.get(&f.bus_to)?;
                    // Find matching branch
                    let (_branch_idx, br) =
                        network.branches.iter().enumerate().find(|(_, br)| {
                            br.in_service
                                && ((br.from_bus == f.bus_from && br.to_bus == f.bus_to)
                                    || (br.from_bus == f.bus_to && br.to_bus == f.bus_from))
                        })?;
                    let x_orig = br.x;
                    let r = br.r;
                    let tap = if br.tap.abs() > 1e-10 { br.tap } else { 1.0 };
                    let shift_rad = br.phase_shift_rad;
                    // Compensation limits: default to [-0.5*|x|, 0.8*|x|] if not specified
                    let x_comp_min = f.x_min.unwrap_or(-0.5 * x_orig.abs());
                    let x_comp_max_raw = f.x_max.unwrap_or(0.8 * x_orig.abs());
                    // Safety: prevent near-zero impedance
                    let x_comp_max = x_comp_max_raw.min(x_orig.abs() - r.abs().max(0.001));
                    // Skip device if bounds are degenerate (|x_orig| too close to |r|)
                    if x_comp_max <= x_comp_min + 1e-6 {
                        return None;
                    }
                    let x_comp_init = f.series_reactance_pu.clamp(x_comp_min, x_comp_max);
                    Some(TcscOpfData {
                        from_idx,
                        to_idx,
                        x_orig,
                        r,
                        tap,
                        shift_rad,
                        x_comp_min,
                        x_comp_max,
                        x_comp_init,
                    })
                })
                .collect()
        } else {
            vec![]
        };
        let n_tcsc = tcsc_devices.len();

        // HVDC dimensions
        let (n_conv, n_dc_bus, conv_ac_bus) = if let Some(h) = hvdc {
            let ac: Vec<usize> = h.converters.iter().map(|c| c.ac_bus_idx).collect();
            (h.n_conv, h.n_dc_bus, ac)
        } else {
            (0, 0, vec![])
        };

        // Reactive reserve detection. The AC OPF allocates per-device
        // `q^qru`/`q^qrd` variables and zonal shortfall slacks only
        // when the network carries at least one `ReserveProduct` with
        // `kind = Reactive` AND capability curves are enforced. When
        // the trigger is off, every new block has size 0 and the
        // layout matches the non-reactive mapping byte-for-byte.
        let reactive_reserves_active = enforce_capability_curves
            && network
                .market_data
                .market_rules
                .as_ref()
                .is_some_and(|rules| {
                    rules
                        .reserve_products
                        .iter()
                        .any(|p| matches!(p.kind, ReserveKind::Reactive))
                });
        let n_dl = dispatchable_load_bus_idx.len();
        let n_producer_q_reserve = if reactive_reserves_active { n_gen } else { 0 };
        let n_consumer_q_reserve = if reactive_reserves_active { n_dl } else { 0 };
        let (n_zone_q_reserve_up_shortfall, n_zone_q_reserve_down_shortfall) =
            if reactive_reserves_active {
                count_reactive_zone_balance_rows(network)
            } else {
                (0, 0)
            };

        // Collect angle-constrained branches (only when explicitly enabled).
        // Default off — matches MATPOWER's opf.ignore_angle_lim = 1 default.
        // Many case files (e.g., ACTIVSg exported from PowerWorld) store
        // angmin = angmax = 0 as the current operating angle, not a binding limit;
        // enforcing those values makes the NLP infeasible.
        //
        // Collected before the variable-offset block so `n_angle_slack` can
        // be included in the layout tail.
        const ANG_LO: f64 = -std::f64::consts::PI;
        const ANG_HI: f64 = std::f64::consts::PI;
        let mut angle_constrained_branches: Vec<(usize, f64, f64)> = Vec::new();
        if enforce_angle_limits {
            for (br_idx, br) in network.branches.iter().enumerate() {
                if !br.in_service {
                    continue;
                }
                let lo = br.angle_diff_min_rad.unwrap_or(f64::NEG_INFINITY);
                let hi = br.angle_diff_max_rad.unwrap_or(f64::INFINITY);
                if lo > ANG_LO || hi < ANG_HI {
                    angle_constrained_branches.push((br_idx, lo, hi));
                }
            }
        }
        let n_ang = angle_constrained_branches.len();

        // Variable layout: [Va(n_va) | Vm(n_vm) | Pg(n_gen) | Qg(n_gen) | qru_pr(n_qr_pr) | qrd_pr(n_qr_pr) | τ(n_tap) | θ_s(n_ps) | b_sw(n_sw) | b_svc(n_svc) | x_comp(n_tcsc) | P_conv(n_conv) | Q_conv(n_conv) | V_dc(n_dc_bus) | I_conv(n_conv) | dis(n_sto) | ch(n_sto) | p_served(n_dl) | q_served(n_dl) | qru_cs(n_qr_cs) | qrd_cs(n_qr_cs) | p_slack_pos(n_bus) | p_slack_neg(n_bus) | q_slack_pos(n_bus) | q_slack_neg(n_bus) | vm_slack_high(n_vm_slack) | vm_slack_low(n_vm_slack) | sigma_from(n_br) | sigma_to(n_br) | zone_qru_shortfall(n_zqru) | zone_qrd_shortfall(n_zqrd) | angle_slack_high(n_ang) | angle_slack_low(n_ang)]
        let qg_offset = n_va + n_vm + n_gen;
        let producer_q_reserve_up_offset = qg_offset + n_gen;
        let producer_q_reserve_down_offset = producer_q_reserve_up_offset + n_producer_q_reserve;
        let tap_offset = producer_q_reserve_down_offset + n_producer_q_reserve;
        let ps_offset = tap_offset + n_tap;
        let sw_offset = ps_offset + n_ps;
        let svc_offset = sw_offset + n_sw;
        let tcsc_offset = svc_offset + n_svc;
        let pconv_offset = tcsc_offset + n_tcsc;
        let qconv_offset = pconv_offset + n_conv;
        let vdc_offset = qconv_offset + n_conv;
        let iconv_offset = vdc_offset + n_dc_bus;
        let n_sto = storage_bus_idx.len();
        let discharge_offset = iconv_offset + n_conv;
        let charge_offset = discharge_offset + n_sto;
        let dl_offset = charge_offset + n_sto;
        let dl_q_offset = dl_offset + n_dl;
        let consumer_q_reserve_up_offset = dl_q_offset + n_dl;
        let consumer_q_reserve_down_offset = consumer_q_reserve_up_offset + n_consumer_q_reserve;
        let n_p_bus_balance_slack = if enable_p_bus_balance_slacks {
            n_bus
        } else {
            0
        };
        let n_q_bus_balance_slack = if enable_q_bus_balance_slacks {
            n_bus
        } else {
            0
        };
        let p_balance_slack_pos_offset = consumer_q_reserve_down_offset + n_consumer_q_reserve;
        let p_balance_slack_neg_offset = p_balance_slack_pos_offset + n_p_bus_balance_slack;
        let q_balance_slack_pos_offset = p_balance_slack_neg_offset + n_p_bus_balance_slack;
        let q_balance_slack_neg_offset = q_balance_slack_pos_offset + n_q_bus_balance_slack;
        let n_vm_slack = if enable_voltage_slacks { n_bus } else { 0 };
        let vm_slack_high_offset = q_balance_slack_neg_offset + n_q_bus_balance_slack;
        let vm_slack_low_offset = vm_slack_high_offset + n_vm_slack;
        let n_branch_thermal_slack = if enable_thermal_limit_slacks {
            constrained_branches.len()
        } else {
            0
        };
        let thermal_slack_from_offset = vm_slack_low_offset + n_vm_slack;
        let thermal_slack_to_offset = thermal_slack_from_offset + n_branch_thermal_slack;
        let zone_q_reserve_up_shortfall_offset = thermal_slack_to_offset + n_branch_thermal_slack;
        let zone_q_reserve_down_shortfall_offset =
            zone_q_reserve_up_shortfall_offset + n_zone_q_reserve_up_shortfall;
        let n_angle_slack = if enable_angle_slacks && !angle_constrained_branches.is_empty() {
            angle_constrained_branches.len()
        } else {
            0
        };
        let angle_slack_high_offset =
            zone_q_reserve_down_shortfall_offset + n_zone_q_reserve_down_shortfall;
        let angle_slack_low_offset = angle_slack_high_offset + n_angle_slack;
        // HVDC point-to-point P variables are appended at the very end of
        // the variable vector so adding them doesn't shift any existing
        // offsets. Each in-service link with a non-degenerate P range
        // contributes one variable.
        let (n_hvdc_p2p_links, hvdc_p2p_from_bus_idx, hvdc_p2p_to_bus_idx, hvdc_p2p_loss_c_pu): (
            usize,
            Vec<usize>,
            Vec<usize>,
            Vec<f64>,
        ) = if let Some(p2p) = hvdc_p2p {
            let from: Vec<usize> = p2p.links.iter().map(|l| l.from_bus_idx).collect();
            let to: Vec<usize> = p2p.links.iter().map(|l| l.to_bus_idx).collect();
            let loss_c: Vec<f64> = p2p.links.iter().map(|l| l.loss_c_pu).collect();
            (p2p.links.len(), from, to, loss_c)
        } else {
            (0, Vec::new(), Vec::new(), Vec::new())
        };
        let hvdc_p2p_offset = angle_slack_low_offset + n_angle_slack;
        let n_var = hvdc_p2p_offset + n_hvdc_p2p_links;

        // Count pq_con rows. The `pq_con` block aggregates every
        // constraint of the form `q_dev − slope·p_dev + sign·q_reserve ∈
        // [lhs_lb, lhs_ub]`, regardless of whether the row comes from
        //   * the sampled D-curve envelope (OPF-06; producers only),
        //   * linear p-q linking for producers and consumers, or
        //   * flat q-headroom with reactive-reserve coupling.
        //
        // All three families share the same row form, sparsity
        // pattern, and residual machinery, so they live in a single
        // contiguous block starting at `pq_con_offset`. Gated on
        // `enforce_capability_curves` — when that flag is false,
        // every family collapses to zero rows and all generators /
        // consumers fall back to flat box bounds.
        let n_pq_producer_dcurve_and_linear: usize = if enforce_capability_curves {
            gen_indices
                .iter()
                .map(|&gi| {
                    let rc = network.generators[gi].reactive_capability.as_ref();
                    let curve_len = rc.map_or(0, |r| r.pq_curve.len());
                    let n_curve = if curve_len >= 2 {
                        2 * (curve_len - 1)
                    } else {
                        0
                    };
                    let n_linear = rc
                        .map(|r| {
                            usize::from(r.pq_linear_equality.is_some())
                                + usize::from(r.pq_linear_upper.is_some())
                                + usize::from(r.pq_linear_lower.is_some())
                        })
                        .unwrap_or(0);
                    n_curve + n_linear
                })
                .sum()
        } else {
            0
        };
        let n_pq_consumer_linear: usize = if enforce_capability_curves {
            network
                .market_data
                .dispatchable_loads
                .iter()
                .filter(|dl| dl.in_service)
                .map(|dl| {
                    usize::from(dl.pq_linear_equality.is_some())
                        + usize::from(dl.pq_linear_upper.is_some())
                        + usize::from(dl.pq_linear_lower.is_some())
                })
                .sum()
        } else {
            0
        };
        // Flat q-headroom rows (eqs 112-113 producers, 122-123 consumers)
        // — two per device (up + down) when reactive reserves are active.
        let n_pq_producer_flat_headroom = 2 * n_producer_q_reserve;
        let n_pq_consumer_flat_headroom = 2 * n_consumer_q_reserve;
        let n_pq_cons = n_pq_producer_dcurve_and_linear
            + n_pq_consumer_linear
            + n_pq_producer_flat_headroom
            + n_pq_consumer_flat_headroom;

        // Constraint layout:
        //   2*n_bus (P+Q balance)
        //   + 2*n_constrained_branches (from+to flow limits)
        //   + n_ang (angle difference constraints)
        //   + n_dc_bus (DC KCL equality constraints, when HVDC augmented)
        //   + n_conv (converter current-definition: P²+Q²-Vm²·I²=0)
        //   + n_conv (DC control equations)
        //   + n_conv (AC control equations)
        //   + n_pq_cons (D-curve piecewise-linear capability constraints)
        let dc_kcl_row_offset = 2 * n_bus + 2 * constrained_branches.len() + n_ang;
        let iconv_eq_row_offset = dc_kcl_row_offset + n_dc_bus;
        let dc_control_row_offset = iconv_eq_row_offset + n_conv;
        let ac_control_row_offset = dc_control_row_offset + n_conv;
        let pq_con_offset = ac_control_row_offset + n_conv;

        // Collect active flowgate and interface indices.
        let flowgate_indices: Vec<usize> = if enforce_flowgates {
            network
                .flowgates
                .iter()
                .enumerate()
                .filter(|(_, fg)| fg.in_service)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };
        let interface_indices: Vec<usize> = if enforce_flowgates {
            network
                .interfaces
                .iter()
                .enumerate()
                .filter(|(_, iface)| iface.in_service && iface.limit_forward_mw > 0.0)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![]
        };
        // Zonal q-reserve balance rows come after the pq_con block
        // and before the DL power-factor equality rows.
        let zone_q_reserve_balance_row_offset = pq_con_offset + n_pq_cons;
        let n_zone_q_reserve_balance_rows =
            n_zone_q_reserve_up_shortfall + n_zone_q_reserve_down_shortfall;

        let mut dispatchable_load_pf_rows = vec![None; n_dl];
        let dl_pf_con_offset = zone_q_reserve_balance_row_offset + n_zone_q_reserve_balance_rows;
        let mut next_dl_pf_row = dl_pf_con_offset;
        for (k, fixed_pf) in dispatchable_load_fixed_power_factor
            .iter()
            .copied()
            .enumerate()
        {
            if fixed_pf {
                dispatchable_load_pf_rows[k] = Some(next_dl_pf_row);
                next_dl_pf_row += 1;
            }
        }
        let n_fg = flowgate_indices.len();
        let n_iface = interface_indices.len();
        let fg_con_offset = next_dl_pf_row;
        let iface_con_offset = fg_con_offset + n_fg;
        let vm_slack_con_offset = iface_con_offset + n_iface;
        let n_con = vm_slack_con_offset + 2 * n_vm_slack;

        Ok(Self {
            n_bus,
            slack_idx,
            gen_indices,
            n_gen,
            bus_gen_map,
            constrained_branches,
            angle_constrained_branches,
            tap_ctrl_branches,
            ps_ctrl_branches,
            n_var,
            n_con,
            pq_con_offset,
            va_offset: 0,
            vm_offset: n_va,
            pg_offset: n_va + n_vm,
            qg_offset,
            producer_q_reserve_up_offset,
            producer_q_reserve_down_offset,
            n_producer_q_reserve,
            tap_offset,
            ps_offset,
            sw_offset,
            n_sw,
            switched_shunt_bus_idx,
            svc_offset,
            n_svc,
            svc_devices,
            tcsc_offset,
            n_tcsc,
            tcsc_devices,
            pconv_offset,
            qconv_offset,
            vdc_offset,
            iconv_offset,
            n_conv,
            n_dc_bus,
            conv_ac_bus,
            dc_kcl_row_offset,
            iconv_eq_row_offset,
            dc_control_row_offset,
            ac_control_row_offset,
            n_sto,
            discharge_offset,
            charge_offset,
            storage_bus_idx,
            n_dl,
            dl_offset,
            dl_q_offset,
            consumer_q_reserve_up_offset,
            consumer_q_reserve_down_offset,
            n_consumer_q_reserve,
            dispatchable_load_bus_idx,
            dispatchable_load_fixed_power_factor,
            dispatchable_load_pf_ratio,
            dispatchable_load_pf_rows,
            n_branch_thermal_slack,
            n_p_bus_balance_slack,
            n_q_bus_balance_slack,
            p_balance_slack_pos_offset,
            p_balance_slack_neg_offset,
            q_balance_slack_pos_offset,
            q_balance_slack_neg_offset,
            n_vm_slack,
            vm_slack_high_offset,
            vm_slack_low_offset,
            thermal_slack_from_offset,
            thermal_slack_to_offset,
            zone_q_reserve_up_shortfall_offset,
            zone_q_reserve_down_shortfall_offset,
            n_zone_q_reserve_up_shortfall,
            n_zone_q_reserve_down_shortfall,
            zone_q_reserve_balance_row_offset,
            flowgate_indices,
            interface_indices,
            fg_con_offset,
            iface_con_offset,
            vm_slack_con_offset,
            n_angle_slack,
            angle_slack_high_offset,
            angle_slack_low_offset,
            n_hvdc_p2p_links,
            hvdc_p2p_offset,
            hvdc_p2p_from_bus_idx,
            hvdc_p2p_to_bus_idx,
            hvdc_p2p_loss_c_pu,
        })
    }

    /// Column index of the HVDC point-to-point P variable for link `k`.
    ///
    /// Panics (debug builds) if `k >= n_hvdc_p2p_links`.
    #[inline]
    pub(crate) fn hvdc_p2p_var(&self, k: usize) -> usize {
        debug_assert!(
            k < self.n_hvdc_p2p_links,
            "hvdc_p2p_var({k}): k out of range (n={})",
            self.n_hvdc_p2p_links
        );
        self.hvdc_p2p_offset + k
    }

    /// True when the mapping has at least one HVDC P2P decision variable.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn has_hvdc_p2p(&self) -> bool {
        self.n_hvdc_p2p_links > 0
    }

    /// Get tap ratio variable index for tap-controllable branch k.
    #[inline]
    pub(crate) fn tap_var(&self, k: usize) -> usize {
        self.tap_offset + k
    }

    /// Get phase-shift variable index for phase-controllable branch k.
    #[inline]
    pub(crate) fn ps_var(&self, k: usize) -> usize {
        self.ps_offset + k
    }

    /// Get switched shunt susceptance variable index for shunt i.
    #[inline]
    pub(crate) fn sw_var(&self, i: usize) -> usize {
        self.sw_offset + i
    }

    /// Get SVC susceptance variable index for device i.
    #[inline]
    pub(crate) fn svc_var(&self, i: usize) -> usize {
        self.svc_offset + i
    }

    /// Get TCSC compensation reactance variable index for device i.
    #[inline]
    pub(crate) fn tcsc_var(&self, i: usize) -> usize {
        self.tcsc_offset + i
    }

    /// Get P_conv variable index for converter k.
    #[inline]
    pub(crate) fn pconv_var(&self, k: usize) -> usize {
        self.pconv_offset + k
    }

    /// Get Q_conv variable index for converter k.
    #[inline]
    pub(crate) fn qconv_var(&self, k: usize) -> usize {
        self.qconv_offset + k
    }

    /// Get V_dc variable index for DC bus d.
    #[inline]
    pub(crate) fn vdc_var(&self, d: usize) -> usize {
        self.vdc_offset + d
    }

    /// Get I_conv variable index for converter k.
    #[inline]
    pub(crate) fn iconv_var(&self, k: usize) -> usize {
        self.iconv_offset + k
    }

    pub(crate) fn dc_control_row(&self, k: usize) -> usize {
        self.dc_control_row_offset + k
    }

    pub(crate) fn ac_control_row(&self, k: usize) -> usize {
        self.ac_control_row_offset + k
    }

    /// Get discharge variable index for storage unit s.
    #[inline]
    pub(crate) fn discharge_var(&self, s: usize) -> usize {
        self.discharge_offset + s
    }

    /// Get charge variable index for storage unit s.
    #[inline]
    pub(crate) fn charge_var(&self, s: usize) -> usize {
        self.charge_offset + s
    }

    /// Get served-real-power variable index for dispatchable load k.
    #[inline]
    pub(crate) fn dl_var(&self, k: usize) -> usize {
        self.dl_offset + k
    }

    /// Get served-reactive-power variable index for dispatchable load k.
    #[inline]
    pub(crate) fn dl_q_var(&self, k: usize) -> usize {
        self.dl_q_offset + k
    }

    // -- Reactive reserves ---------------------------------------------

    /// True when the mapping has allocated reactive reserve variable
    /// blocks (i.e. the network has at least one `kind = Reactive`
    /// reserve product and capability curves are enforced).
    #[inline]
    pub(crate) fn reactive_reserves_active(&self) -> bool {
        self.n_producer_q_reserve > 0 || self.n_consumer_q_reserve > 0
    }

    /// Expected count of rows in the `pq_con` block. Used as a sanity
    /// check against the row builder in `reactive_reserves.rs` — the
    /// two must stay in sync or variable/row indexing becomes corrupt.
    #[inline]
    pub(crate) fn n_con_pq_cons_expected(&self) -> usize {
        self.zone_q_reserve_balance_row_offset - self.pq_con_offset
    }

    /// Column index of the producer reactive-up reserve variable
    /// `q^qru_j` for local generator `j`. Panics if q-reserves are
    /// inactive — callers should guard on [`Self::reactive_reserves_active`].
    #[inline]
    pub(crate) fn producer_q_reserve_up_var(&self, j: usize) -> usize {
        debug_assert!(
            j < self.n_producer_q_reserve,
            "producer_q_reserve_up_var({j}): j out of range (n={})",
            self.n_producer_q_reserve
        );
        self.producer_q_reserve_up_offset + j
    }

    /// Column index of the producer reactive-down reserve variable
    /// `q^qrd_j` for local generator `j`.
    #[inline]
    pub(crate) fn producer_q_reserve_down_var(&self, j: usize) -> usize {
        debug_assert!(
            j < self.n_producer_q_reserve,
            "producer_q_reserve_down_var({j}): j out of range (n={})",
            self.n_producer_q_reserve
        );
        self.producer_q_reserve_down_offset + j
    }

    /// Column index of the consumer reactive-up reserve variable
    /// `q^qru_k` for local dispatchable load `k`.
    #[inline]
    pub(crate) fn consumer_q_reserve_up_var(&self, k: usize) -> usize {
        debug_assert!(
            k < self.n_consumer_q_reserve,
            "consumer_q_reserve_up_var({k}): k out of range (n={})",
            self.n_consumer_q_reserve
        );
        self.consumer_q_reserve_up_offset + k
    }

    /// Column index of the consumer reactive-down reserve variable
    /// `q^qrd_k` for local dispatchable load `k`.
    #[inline]
    pub(crate) fn consumer_q_reserve_down_var(&self, k: usize) -> usize {
        debug_assert!(
            k < self.n_consumer_q_reserve,
            "consumer_q_reserve_down_var({k}): k out of range (n={})",
            self.n_consumer_q_reserve
        );
        self.consumer_q_reserve_down_offset + k
    }

    /// Column index of the `i`-th zonal reactive-up shortfall slack
    /// (`q^qru,+_n`). Valid for `i < n_zone_q_reserve_up_shortfall`.
    #[inline]
    pub(crate) fn zone_q_reserve_up_shortfall_var(&self, i: usize) -> usize {
        debug_assert!(i < self.n_zone_q_reserve_up_shortfall);
        self.zone_q_reserve_up_shortfall_offset + i
    }

    /// Column index of the `i`-th zonal reactive-down shortfall slack
    /// (`q^qrd,+_n`). Valid for `i < n_zone_q_reserve_down_shortfall`.
    #[inline]
    pub(crate) fn zone_q_reserve_down_shortfall_var(&self, i: usize) -> usize {
        debug_assert!(i < self.n_zone_q_reserve_down_shortfall);
        self.zone_q_reserve_down_shortfall_offset + i
    }

    /// Row index of the `i`-th zonal q-reserve balance row. Up-direction
    /// rows are indexed `[0, n_zone_q_reserve_up_shortfall)`; down rows
    /// follow them at `[n_zone_q_reserve_up_shortfall,
    /// n_zone_q_reserve_up_shortfall + n_zone_q_reserve_down_shortfall)`.
    #[inline]
    pub(crate) fn zone_q_reserve_balance_row(&self, i: usize) -> usize {
        self.zone_q_reserve_balance_row_offset + i
    }

    #[inline]
    pub(crate) fn has_thermal_limit_slacks(&self) -> bool {
        self.n_branch_thermal_slack > 0
    }

    #[inline]
    pub(crate) fn has_p_bus_balance_slacks(&self) -> bool {
        self.n_p_bus_balance_slack > 0
    }

    #[inline]
    pub(crate) fn has_q_bus_balance_slacks(&self) -> bool {
        self.n_q_bus_balance_slack > 0
    }

    #[inline]
    pub(crate) fn p_balance_slack_pos_var(&self, bus: usize) -> usize {
        self.p_balance_slack_pos_offset + bus
    }

    #[inline]
    pub(crate) fn p_balance_slack_neg_var(&self, bus: usize) -> usize {
        self.p_balance_slack_neg_offset + bus
    }

    #[inline]
    pub(crate) fn q_balance_slack_pos_var(&self, bus: usize) -> usize {
        self.q_balance_slack_pos_offset + bus
    }

    #[inline]
    pub(crate) fn q_balance_slack_neg_var(&self, bus: usize) -> usize {
        self.q_balance_slack_neg_offset + bus
    }

    #[inline]
    pub(crate) fn has_voltage_slacks(&self) -> bool {
        self.n_vm_slack > 0
    }

    #[inline]
    pub(crate) fn vm_slack_high_var(&self, bus: usize) -> usize {
        self.vm_slack_high_offset + bus
    }

    #[inline]
    pub(crate) fn vm_slack_low_var(&self, bus: usize) -> usize {
        self.vm_slack_low_offset + bus
    }

    #[inline]
    pub(crate) fn thermal_slack_from_var(&self, k: usize) -> usize {
        self.thermal_slack_from_offset + k
    }

    #[inline]
    pub(crate) fn thermal_slack_to_var(&self, k: usize) -> usize {
        self.thermal_slack_to_offset + k
    }

    #[inline]
    pub(crate) fn has_angle_slacks(&self) -> bool {
        self.n_angle_slack > 0
    }

    #[inline]
    pub(crate) fn angle_slack_high_var(&self, ai: usize) -> usize {
        self.angle_slack_high_offset + ai
    }

    #[inline]
    pub(crate) fn angle_slack_low_var(&self, ai: usize) -> usize {
        self.angle_slack_low_offset + ai
    }

    /// Get Va index for bus i (returns None for slack).
    #[inline]
    pub(crate) fn va_var(&self, bus: usize) -> Option<usize> {
        if bus == self.slack_idx {
            None
        } else if bus < self.slack_idx {
            Some(self.va_offset + bus)
        } else {
            Some(self.va_offset + bus - 1)
        }
    }

    /// Get Vm index for bus i.
    #[inline]
    pub(crate) fn vm_var(&self, bus: usize) -> usize {
        self.vm_offset + bus
    }

    /// Get Pg index for local gen j.
    #[inline]
    pub(crate) fn pg_var(&self, j: usize) -> usize {
        self.pg_offset + j
    }

    /// Get Qg index for local gen j.
    #[inline]
    pub(crate) fn qg_var(&self, j: usize) -> usize {
        self.qg_offset + j
    }

    /// Unpack x into (va, vm, pg, qg) vectors.
    pub(crate) fn extract_voltages_and_dispatch<'a>(
        &self,
        x: &'a [f64],
    ) -> (Vec<f64>, &'a [f64], &'a [f64], &'a [f64]) {
        // Reconstruct full va including slack = 0
        let mut va = vec![0.0; self.n_bus];
        for (i, va_i) in va.iter_mut().enumerate() {
            if let Some(idx) = self.va_var(i) {
                *va_i = x[idx];
            }
            // slack stays 0
        }
        let vm = &x[self.vm_offset..self.vm_offset + self.n_bus];
        let pg = &x[self.pg_offset..self.pg_offset + self.n_gen];
        // Qg is contiguous at qg_offset even when producer q-reserve
        // variables are inserted after it — the q-reserve block starts
        // at `qg_offset + n_gen`, so the Qg slice is still valid.
        let qg = &x[self.qg_offset..self.qg_offset + self.n_gen];
        (va, vm, pg, qg)
    }
}

/// Count distinct zonal reactive reserve requirements in the network,
/// returning `(n_up_shortfall_slacks, n_down_shortfall_slacks)`.
///
/// Walks `network.market_data.reserve_zones` and their
/// `zonal_requirements`, filtering to products whose definition in
/// `market_rules.reserve_products` has `kind = Reactive`. Each
/// `(zone, product)` pair with a non-negative requirement contributes
/// exactly one shortfall slack column and one balance row. Used by
/// [`AcOpfMapping::new`] to size the zonal-balance variable block and
/// row block.
fn count_reactive_zone_balance_rows(network: &Network) -> (usize, usize) {
    let Some(rules) = network.market_data.market_rules.as_ref() else {
        return (0, 0);
    };
    let mut n_up = 0usize;
    let mut n_down = 0usize;
    for zone in &network.market_data.reserve_zones {
        for req in &zone.zonal_requirements {
            let Some(product) = rules
                .reserve_products
                .iter()
                .find(|p| p.id == req.product_id)
            else {
                continue;
            };
            if !matches!(product.kind, ReserveKind::Reactive) {
                continue;
            }
            match product.direction {
                surge_network::market::ReserveDirection::Up => n_up += 1,
                surge_network::market::ReserveDirection::Down => n_down += 1,
            }
        }
    }
    (n_up, n_down)
}
