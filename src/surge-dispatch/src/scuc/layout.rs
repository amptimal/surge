// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! SCUC column layout and active-input planning.

use std::collections::HashMap;

use surge_network::Network;
use surge_network::market::DispatchableLoad;
use surge_network::market::{ReserveProduct, SystemReserveRequirement, ZonalReserveRequirement};
use tracing::info;

use crate::common::layout::DispatchOffsets;
use crate::common::reserves::ReserveLpLayout;
use crate::common::spec::DispatchProblemSpec;

/// Named SCUC variable layout for a single hour block plus post-hourly bases.
pub(super) struct ScucLayout {
    pub dispatch: DispatchOffsets,
    pub commitment: usize,
    pub startup: usize,
    pub shutdown: usize,
    pub startup_delta: usize,
    pub plc_lambda: usize,
    pub plc_sos2_binary: usize,
    pub regulation_mode: usize,
    pub foz_delta: usize,
    pub foz_phi: usize,
    pub foz_rho: usize,
    pub ph_mode: usize,
    pub pb_curtailment_bus: usize,
    pub pb_excess_bus: usize,
    pub pb_curtailment_seg: usize,
    pub pb_excess_seg: usize,
    pub branch_lower_slack: usize,
    pub branch_upper_slack: usize,
    pub flowgate_lower_slack: usize,
    pub flowgate_upper_slack: usize,
    pub interface_lower_slack: usize,
    pub interface_upper_slack: usize,
    pub headroom_slack: usize,
    pub footroom_slack: usize,
    pub ramp_up_slack: usize,
    pub ramp_down_slack: usize,
    pub angle_diff_lower_slack: usize,
    pub angle_diff_upper_slack: usize,
    pub n_angle_diff_rows: usize,
    /// Branch on/off binary `u^on_jt` for AC branches. Sized as
    /// `n_ac_branches` per period. When the `allow_branch_switching`
    /// policy is `false` the bounds layer pins every branch_commitment
    /// column to the network's static `in_service` flag, leaving the
    /// LP layout unchanged but the variables effectively absent. When
    /// `true`, the variables are free in `{0, 1}` and the security
    /// loop adds connectivity cuts over them whenever a solved
    /// switching pattern disconnects the bus-branch graph.
    pub branch_commitment: usize,
    /// Branch startup binary `u^su_jt` (close-circuit transition). Sized
    /// as `n_ac_branches` per period. State evolution row links it to
    /// `branch_commitment[t] - branch_commitment[t-1]`.
    pub branch_startup: usize,
    /// Branch shutdown binary `u^sd_jt` (open-circuit transition).
    pub branch_shutdown: usize,
    /// Switchable-branch flow variable `pf_l` per AC branch per period.
    /// Sized as `n_ac_branches` per period when
    /// `allow_branch_switching = true`, zero otherwise. The Big-M flow
    /// definition rows in
    /// `scuc::rows::build_branch_flow_definition_rows` anchor on this
    /// column block, and the SCUC KCL rewrite swaps the y-bus `b·Δθ`
    /// contribution for `±pf_l` injection at each branch endpoint.
    pub branch_flow: usize,
    /// Number of `pf_l` columns per period (= n_ac_branches when
    /// switching is allowed, 0 otherwise). Used by the row and bounds
    /// builders to gate the Big-M row family and KCL rewrite.
    pub n_branch_flow_per_hour: usize,
}

impl ScucLayout {
    #[allow(clippy::too_many_arguments)]
    pub fn build_prefix(
        n_bus: usize,
        n_gen: usize,
        n_delta_per_hour: usize,
        use_plc: bool,
        n_bp: usize,
        n_storage: usize,
        n_sto_dis_epi: usize,
        n_sto_ch_epi: usize,
        n_hvdc_vars: usize,
        n_pwl_gen: usize,
        n_dl: usize,
        n_vbid: usize,
        n_block_vars_per_hour: usize,
        n_reg_vars: usize,
    ) -> Self {
        let n_sbp = if use_plc && n_bp > 2 { n_bp - 2 } else { 0 };
        let theta = 0;
        let pg = theta + n_bus;
        let commitment = pg + n_gen;
        let startup = commitment + n_gen;
        let shutdown = startup + n_gen;
        let startup_delta = shutdown + n_gen;
        let plc_lambda = startup_delta + n_delta_per_hour;
        let plc_sos2_binary = plc_lambda + if use_plc { n_gen * n_bp } else { 0 };
        let sto_ch = plc_sos2_binary + if use_plc { n_gen * n_sbp } else { 0 };
        let sto_dis = sto_ch + n_storage;
        let sto_soc = sto_dis + n_storage;
        let sto_epi_dis = sto_soc + n_storage;
        let sto_epi_ch = sto_epi_dis + n_sto_dis_epi;
        let hvdc = sto_epi_ch + n_sto_ch_epi;
        let e_g = hvdc + n_hvdc_vars;
        let dl = e_g + n_pwl_gen;
        let vbid = dl + n_dl;
        let block = vbid + n_vbid;
        let regulation_mode = block + n_block_vars_per_hour;
        let reserve = regulation_mode + n_reg_vars;

        Self {
            dispatch: DispatchOffsets {
                theta,
                pg,
                sto_ch,
                sto_dis,
                sto_soc,
                sto_epi_dis,
                sto_epi_ch,
                hvdc,
                e_g,
                dl,
                vbid,
                block,
                reserve,
                block_reserve: 0,
                n_vars: 0,
            },
            commitment,
            startup,
            shutdown,
            startup_delta,
            plc_lambda,
            plc_sos2_binary,
            regulation_mode,
            foz_delta: 0,
            foz_phi: 0,
            foz_rho: 0,
            ph_mode: 0,
            pb_curtailment_bus: 0,
            pb_excess_bus: 0,
            pb_curtailment_seg: 0,
            pb_excess_seg: 0,
            branch_lower_slack: 0,
            branch_upper_slack: 0,
            flowgate_lower_slack: 0,
            flowgate_upper_slack: 0,
            interface_lower_slack: 0,
            interface_upper_slack: 0,
            headroom_slack: 0,
            footroom_slack: 0,
            ramp_up_slack: 0,
            ramp_down_slack: 0,
            angle_diff_lower_slack: 0,
            angle_diff_upper_slack: 0,
            n_angle_diff_rows: 0,
            branch_commitment: 0,
            branch_startup: 0,
            branch_shutdown: 0,
            branch_flow: 0,
            n_branch_flow_per_hour: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_post_reserve(
        &mut self,
        reserve_var_count: usize,
        n_blk_res_vars_per_hour: usize,
        n_foz_delta: usize,
        n_foz_phi: usize,
        n_foz_rho: usize,
        n_ph_mode_vars_per_hour: usize,
        n_bus: usize,
        n_pb_curt_segs: usize,
        n_pb_excess_segs: usize,
        n_branch_flow: usize,
        n_fg_rows: usize,
        n_iface_rows: usize,
        n_gen: usize,
        n_angle_diff_rows_arg: usize,
        n_ac_branches: usize,
        n_branch_switching_flow_per_hour: usize,
    ) {
        self.dispatch.block_reserve = self.dispatch.reserve + reserve_var_count;
        self.foz_delta = self.dispatch.block_reserve + n_blk_res_vars_per_hour;
        self.foz_phi = self.foz_delta + n_foz_delta;
        self.foz_rho = self.foz_phi + n_foz_phi;
        self.ph_mode = self.foz_rho + n_foz_rho;
        self.pb_curtailment_bus = self.ph_mode + n_ph_mode_vars_per_hour;
        self.pb_excess_bus = self.pb_curtailment_bus + n_bus;
        self.pb_curtailment_seg = self.pb_excess_bus + n_bus;
        self.pb_excess_seg = self.pb_curtailment_seg + n_pb_curt_segs;
        self.branch_lower_slack = self.pb_excess_seg + n_pb_excess_segs;
        self.branch_upper_slack = self.branch_lower_slack + n_branch_flow;
        self.flowgate_lower_slack = self.branch_upper_slack + n_branch_flow;
        self.flowgate_upper_slack = self.flowgate_lower_slack + n_fg_rows;
        self.interface_lower_slack = self.flowgate_upper_slack + n_fg_rows;
        self.interface_upper_slack = self.interface_lower_slack + n_iface_rows;
        self.headroom_slack = self.interface_upper_slack + n_iface_rows;
        self.footroom_slack = self.headroom_slack + n_gen;
        self.ramp_up_slack = self.footroom_slack + n_gen;
        self.ramp_down_slack = self.ramp_up_slack + n_gen;
        self.angle_diff_lower_slack = self.ramp_down_slack + n_gen;
        self.angle_diff_upper_slack = self.angle_diff_lower_slack + n_angle_diff_rows_arg;
        self.n_angle_diff_rows = n_angle_diff_rows_arg;
        self.branch_commitment = self.angle_diff_upper_slack + n_angle_diff_rows_arg;
        self.branch_startup = self.branch_commitment + n_ac_branches;
        self.branch_shutdown = self.branch_startup + n_ac_branches;
        self.branch_flow = self.branch_shutdown + n_ac_branches;
        self.n_branch_flow_per_hour = n_branch_switching_flow_per_hour;
        self.dispatch.n_vars = self.branch_flow + n_branch_switching_flow_per_hour;
    }

    pub fn reserve_base(&self) -> usize {
        self.dispatch.reserve
    }

    pub fn vars_per_hour(&self) -> usize {
        self.dispatch.n_vars
    }

    pub fn hour_col_base(&self, hour: usize) -> usize {
        hour * self.dispatch.n_vars
    }

    pub fn col(&self, hour: usize, offset: usize) -> usize {
        self.hour_col_base(hour) + offset
    }

    pub fn theta_col(&self, hour: usize, bus_idx: usize) -> usize {
        self.col(hour, self.dispatch.theta + bus_idx)
    }

    pub fn pg_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.dispatch.pg + gen_idx)
    }

    pub fn commitment_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.commitment + gen_idx)
    }

    pub fn startup_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.startup + gen_idx)
    }

    pub fn shutdown_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.shutdown + gen_idx)
    }

    pub fn storage_charge_col(&self, hour: usize, storage_idx: usize) -> usize {
        self.col(hour, self.dispatch.sto_ch + storage_idx)
    }

    pub fn storage_discharge_col(&self, hour: usize, storage_idx: usize) -> usize {
        self.col(hour, self.dispatch.sto_dis + storage_idx)
    }

    pub fn storage_soc_col(&self, hour: usize, storage_idx: usize) -> usize {
        self.col(hour, self.dispatch.sto_soc + storage_idx)
    }

    pub fn pb_curtailment_bus_col(&self, hour: usize, bus_idx: usize) -> usize {
        self.col(hour, self.pb_curtailment_bus + bus_idx)
    }

    pub fn pb_excess_bus_col(&self, hour: usize, bus_idx: usize) -> usize {
        self.col(hour, self.pb_excess_bus + bus_idx)
    }

    pub fn pb_curtailment_seg_col(&self, hour: usize, seg_idx: usize) -> usize {
        self.col(hour, self.pb_curtailment_seg + seg_idx)
    }

    pub fn pb_excess_seg_col(&self, hour: usize, seg_idx: usize) -> usize {
        self.col(hour, self.pb_excess_seg + seg_idx)
    }

    pub fn branch_lower_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.branch_lower_slack + row_idx)
    }

    pub fn branch_upper_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.branch_upper_slack + row_idx)
    }

    pub fn flowgate_lower_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.flowgate_lower_slack + row_idx)
    }

    pub fn flowgate_upper_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.flowgate_upper_slack + row_idx)
    }

    pub fn interface_lower_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.interface_lower_slack + row_idx)
    }

    pub fn interface_upper_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.interface_upper_slack + row_idx)
    }

    pub fn headroom_slack_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.headroom_slack + gen_idx)
    }

    pub fn footroom_slack_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.footroom_slack + gen_idx)
    }

    pub fn ramp_up_slack_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.ramp_up_slack + gen_idx)
    }

    pub fn ramp_down_slack_col(&self, hour: usize, gen_idx: usize) -> usize {
        self.col(hour, self.ramp_down_slack + gen_idx)
    }

    pub fn angle_diff_lower_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.angle_diff_lower_slack + row_idx)
    }

    pub fn angle_diff_upper_slack_col(&self, hour: usize, row_idx: usize) -> usize {
        self.col(hour, self.angle_diff_upper_slack + row_idx)
    }

    /// Branch on/off binary `u^on_jt` for AC branch `branch_local_idx` at
    /// hour `hour`. Eqs (48), (53)-(54), (59)-(60). The local index is the
    /// position of the branch within the network's `ac_branch_indices`
    /// (built by `branch_layout_metadata` below) — NOT the global
    /// `network.branches[]` index.
    pub fn branch_commitment_col(&self, hour: usize, branch_local_idx: usize) -> usize {
        self.col(hour, self.branch_commitment + branch_local_idx)
    }

    /// Branch close-circuit (startup) binary `u^su_jt`. Eqs (49), (53).
    pub fn branch_startup_col(&self, hour: usize, branch_local_idx: usize) -> usize {
        self.col(hour, self.branch_startup + branch_local_idx)
    }

    /// Branch open-circuit (shutdown) binary `u^sd_jt`. Eqs (50), (54).
    pub fn branch_shutdown_col(&self, hour: usize, branch_local_idx: usize) -> usize {
        self.col(hour, self.branch_shutdown + branch_local_idx)
    }

    /// Switchable-branch active-power flow variable `pf_l_t`. Only
    /// allocated when `allow_branch_switching = true` (see
    /// `build_layout_plan`).
    pub fn branch_flow_col(&self, hour: usize, branch_local_idx: usize) -> usize {
        debug_assert!(
            self.n_branch_flow_per_hour > 0,
            "branch_flow_col called without allow_branch_switching"
        );
        self.col(hour, self.branch_flow + branch_local_idx)
    }

    pub fn penalty_slack_base(&self, n_hours: usize) -> usize {
        self.dispatch.n_vars * n_hours
    }
}

pub(super) struct ScucActiveInputs<'a> {
    pub dl_list: Vec<&'a DispatchableLoad>,
    pub dl_orig_idx: Vec<usize>,
    pub active_vbids: Vec<usize>,
    pub reserve_layout: ReserveLpLayout,
    pub has_reg_products: bool,
}

pub(super) struct ScucDrActivationInfo {
    pub load_idx: usize,
    pub notification_periods: usize,
    pub min_duration_periods: usize,
}

pub(super) struct ScucFozGenInfo {
    pub gen_idx: usize,
    pub segments: Vec<(f64, f64)>,
    pub zones: Vec<(f64, f64)>,
    pub max_transit: Vec<usize>,
    pub delta_local_off: usize,
    pub phi_local_off: usize,
    pub rho_local_off: usize,
}

pub(super) struct ScucPhModeInfo {
    pub storage_idx: usize,
    pub dis_max_mw: f64,
    pub ch_max_mw: f64,
    pub min_gen_run: usize,
    pub min_pump_run: usize,
    pub p2g_delay: usize,
    pub g2p_delay: usize,
    pub max_pump_starts: Option<u32>,
    pub m_gen_local_off: usize,
    pub m_pump_local_off: usize,
}

pub(super) struct ScucPhHeadInfo {
    pub storage_idx: usize,
    pub breakpoints: Vec<(f64, f64)>,
}

pub(super) struct ScucLayoutPlan<'a> {
    pub layout: ScucLayout,
    pub active: ScucActiveInputs<'a>,
    pub dl_activation_infos: Vec<ScucDrActivationInfo>,
    pub foz_gens: Vec<ScucFozGenInfo>,
    pub ph_mode_infos: Vec<ScucPhModeInfo>,
    pub ph_head_infos: Vec<ScucPhHeadInfo>,
    pub n_pb_curt_segs: usize,
    pub n_pb_excess_segs: usize,
    pub vars_per_hour: usize,
}

pub(super) struct ScucLayoutPlanInput<'a> {
    pub network: &'a Network,
    pub spec: &'a DispatchProblemSpec<'a>,
    pub has_prev_dispatch: bool,
    pub reserve_products: &'a [ReserveProduct],
    pub system_reserve_requirements: &'a [SystemReserveRequirement],
    pub zonal_reserve_requirements: &'a [ZonalReserveRequirement],
    pub gen_indices: &'a [usize],
    pub storage_gen_local: &'a [(usize, usize, usize)],
    pub n_bus: usize,
    pub n_gen: usize,
    pub n_delta_per_hour: usize,
    pub use_plc: bool,
    pub n_bp: usize,
    pub n_storage: usize,
    pub n_sto_dis_epi: usize,
    pub n_sto_ch_epi: usize,
    pub n_hvdc_vars: usize,
    pub n_pwl_gen: usize,
    pub n_block_vars_per_hour: usize,
    pub is_block_mode: bool,
    pub has_per_block_reserves: bool,
    pub n_branch_flow: usize,
    pub n_fg_rows: usize,
    pub n_iface_rows: usize,
    pub n_angle_diff_rows: usize,
}

#[allow(clippy::too_many_lines)]
pub(super) fn build_layout_plan<'a>(input: ScucLayoutPlanInput<'a>) -> ScucLayoutPlan<'a> {
    let ScucLayoutPlanInput {
        network,
        spec,
        has_prev_dispatch,
        reserve_products,
        system_reserve_requirements,
        zonal_reserve_requirements,
        gen_indices,
        storage_gen_local,
        n_bus,
        n_gen,
        n_delta_per_hour,
        use_plc,
        n_bp,
        n_storage,
        n_sto_dis_epi,
        n_sto_ch_epi,
        n_hvdc_vars,
        n_pwl_gen,
        n_block_vars_per_hour,
        is_block_mode,
        has_per_block_reserves,
        n_branch_flow,
        n_fg_rows,
        n_iface_rows,
        n_angle_diff_rows: _,
    } = input;

    let dl_with_idx: Vec<(usize, &DispatchableLoad)> = spec
        .dispatchable_loads
        .iter()
        .enumerate()
        .filter(|(_, dl)| dl.in_service)
        .collect();
    let dl_list: Vec<&DispatchableLoad> = dl_with_idx.iter().map(|(_, dl)| *dl).collect();
    let dl_orig_idx: Vec<usize> = dl_with_idx.iter().map(|(i, _)| *i).collect();
    let n_dl = dl_list.len();

    let dl_activation_infos: Vec<ScucDrActivationInfo> = dl_list
        .iter()
        .enumerate()
        .filter_map(|(load_idx, dl)| {
            let notification_periods = if dl.dispatch_notification_minutes > 0.0 {
                (dl.dispatch_notification_minutes / (60.0 * spec.dt_hours)).ceil() as usize
            } else {
                0
            };
            let min_duration_periods = if dl.min_duration_hours > 0.0 {
                (dl.min_duration_hours / spec.dt_hours).ceil() as usize
            } else {
                0
            };
            (notification_periods > 0 || min_duration_periods > 1).then_some(ScucDrActivationInfo {
                load_idx,
                notification_periods,
                min_duration_periods,
            })
        })
        .collect();

    let active_vbids: Vec<usize> = spec
        .virtual_bids
        .iter()
        .enumerate()
        .filter(|(_, vb)| vb.in_service)
        .map(|(i, _)| i)
        .collect();
    let n_vbid = active_vbids.len();
    let generator_bus_numbers: Vec<u32> = gen_indices
        .iter()
        .map(|&gi| network.generators[gi].bus)
        .collect();

    let declared_reg_products =
        is_block_mode && reserve_products.iter().any(|p| p.id.starts_with("reg"));
    let n_reg_vars = if declared_reg_products { n_gen } else { 0 };
    let mut layout = ScucLayout::build_prefix(
        n_bus,
        n_gen,
        n_delta_per_hour,
        use_plc,
        n_bp,
        n_storage,
        n_sto_dis_epi,
        n_sto_ch_epi,
        n_hvdc_vars,
        n_pwl_gen,
        n_dl,
        n_vbid,
        n_block_vars_per_hour,
        n_reg_vars,
    );

    let reserve_layout = crate::common::reserves::build_layout(
        reserve_products,
        system_reserve_requirements,
        zonal_reserve_requirements,
        spec.ramp_sharing,
        spec.generator_area,
        &generator_bus_numbers,
        n_gen,
        0,
        n_dl,
        layout.reserve_base(),
        has_prev_dispatch,
    );
    let has_reg_products = declared_reg_products
        && reserve_layout
            .products
            .iter()
            .any(|product| product.product.id.starts_with("reg"));
    let n_blk_res_vars_per_hour = if has_per_block_reserves {
        n_block_vars_per_hour * reserve_layout.products.len()
    } else {
        0
    };

    let mut foz_gens = Vec::new();
    let mut n_foz_delta = 0usize;
    let mut n_foz_phi = 0usize;
    let mut n_foz_rho = 0usize;
    if spec.enforce_forbidden_zones {
        let dt_min = spec.dt_hours * 60.0;
        for (gen_idx, &network_gen_idx) in gen_indices.iter().enumerate() {
            let generator = &network.generators[network_gen_idx];
            let fz = generator
                .commitment
                .as_ref()
                .map(|c| &c.forbidden_zones[..])
                .unwrap_or(&[]);
            if fz.is_empty() {
                continue;
            }

            let mut zones = fz.to_vec();
            zones.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            let mut segments = Vec::with_capacity(zones.len() + 1);
            let mut prev_hi = generator.pmin;
            for &(lo, hi) in &zones {
                if lo > prev_hi + 1e-6 {
                    segments.push((prev_hi, lo));
                }
                prev_hi = hi;
            }
            if generator.pmax > prev_hi + 1e-6 {
                segments.push((prev_hi, generator.pmax));
            }

            let ramp_mw_per_min = generator.ramp_up_avg_mw_per_min().unwrap_or(f64::MAX);
            let max_transit: Vec<usize> = zones
                .iter()
                .map(|&(lo, hi)| {
                    let width = hi - lo;
                    if let Some(override_periods) = spec.foz_max_transit_periods {
                        override_periods
                    } else if ramp_mw_per_min >= f64::MAX || dt_min * ramp_mw_per_min <= 0.0 {
                        0
                    } else {
                        let ramp_per_interval = ramp_mw_per_min * dt_min;
                        if width <= ramp_per_interval {
                            0
                        } else {
                            (width / ramp_per_interval).ceil() as usize
                        }
                    }
                })
                .collect();
            let n_segments = segments.len();
            let n_zones = zones.len();

            foz_gens.push(ScucFozGenInfo {
                gen_idx,
                segments,
                zones,
                max_transit,
                delta_local_off: n_foz_delta,
                phi_local_off: n_foz_phi,
                rho_local_off: n_foz_rho,
            });
            n_foz_delta += n_segments;
            n_foz_phi += n_zones;
            n_foz_rho += n_zones;
        }
        if !foz_gens.is_empty() {
            info!(
                n_foz_gens = foz_gens.len(),
                n_foz_delta, n_foz_phi, "SCUC: forbidden operating zones enabled"
            );
        }
    }

    let gi_to_storage_local: HashMap<usize, usize> = storage_gen_local
        .iter()
        .map(|&(storage_idx, _, network_gen_idx)| (network_gen_idx, storage_idx))
        .collect();
    let mut ph_mode_infos = Vec::new();
    let mut n_ph_mode_vars_per_hour = 0usize;
    for constraint in spec.ph_mode_constraints {
        if let Some(&storage_idx) = gi_to_storage_local.get(&constraint.gen_index) {
            let generator = &network.generators[constraint.gen_index];
            ph_mode_infos.push(ScucPhModeInfo {
                storage_idx,
                dis_max_mw: generator.discharge_mw_max(),
                ch_max_mw: generator.charge_mw_max(),
                min_gen_run: constraint.min_gen_run_periods,
                min_pump_run: constraint.min_pump_run_periods,
                p2g_delay: constraint.pump_to_gen_periods,
                g2p_delay: constraint.gen_to_pump_periods,
                max_pump_starts: constraint.max_pump_starts,
                m_gen_local_off: n_ph_mode_vars_per_hour,
                m_pump_local_off: n_ph_mode_vars_per_hour + 1,
            });
            n_ph_mode_vars_per_hour += 2;
        }
    }

    let ph_head_infos: Vec<ScucPhHeadInfo> = spec
        .ph_head_curves
        .iter()
        .filter_map(|curve| {
            gi_to_storage_local
                .get(&curve.gen_index)
                .copied()
                .filter(|_| curve.breakpoints.len() >= 2)
                .map(|storage_idx| ScucPhHeadInfo {
                    storage_idx,
                    breakpoints: curve.breakpoints.clone(),
                })
        })
        .collect();
    let n_ph_head_rows_per_hour: usize = ph_head_infos
        .iter()
        .map(|unit| {
            unit.breakpoints
                .windows(2)
                .filter(|pair| (pair[1].0 - pair[0].0).abs() >= 1e-12)
                .count()
        })
        .sum();

    let n_pb_curt_segs = spec.power_balance_penalty.curtailment.len();
    let n_pb_excess_segs = spec.power_balance_penalty.excess.len();
    let n_ac_branches = network.branches.len();
    // Allocate one `pf_l` flow column per AC branch per period only
    // when switching is enabled. Keeping the block size at 0 in the
    // default mode means the non-switching layout is untouched.
    let n_branch_switching_flow_per_hour = if spec.allow_branch_switching {
        n_ac_branches
    } else {
        0
    };
    layout.finish_post_reserve(
        reserve_layout.n_reserve_vars,
        n_blk_res_vars_per_hour,
        n_foz_delta,
        n_foz_phi,
        n_foz_rho,
        n_ph_mode_vars_per_hour,
        n_bus,
        n_pb_curt_segs,
        n_pb_excess_segs,
        n_branch_flow,
        n_fg_rows,
        n_iface_rows,
        n_gen,
        input.n_angle_diff_rows,
        n_ac_branches,
        n_branch_switching_flow_per_hour,
    );

    if !ph_mode_infos.is_empty() || !ph_head_infos.is_empty() {
        info!(
            n_ph_mode = ph_mode_infos.len(),
            n_ph_mode_vars_per_hour,
            n_ph_head = ph_head_infos.len(),
            n_ph_head_rows_per_hour,
            "SCUC: pumped-hydro constraints enabled"
        );
    }

    ScucLayoutPlan {
        vars_per_hour: layout.vars_per_hour(),
        layout,
        active: ScucActiveInputs {
            dl_list,
            dl_orig_idx,
            active_vbids,
            reserve_layout,
            has_reg_products,
        },
        dl_activation_infos,
        foz_gens,
        ph_mode_infos,
        ph_head_infos,
        n_pb_curt_segs,
        n_pb_excess_segs,
    }
}
